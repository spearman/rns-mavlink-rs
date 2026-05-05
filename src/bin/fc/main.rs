use std::net::SocketAddr;
use std::process;
use std::sync::Arc;

use clap::Parser;
use kaonic_reticulum::KaonicCtrlInterface;
use log;
use tokio::sync::RwLock;

use reticulum::destination::{DestinationName, SingleInputDestination};
use reticulum::identity::PrivateIdentity;
use reticulum::iface::udp::UdpInterface;
use reticulum::transport::{Transport, TransportConfig};

use rns_mavlink;

use crate::dashboard::FcAppState;

mod dashboard;

const DEFAULT_HTTP_BIND: &str = "0.0.0.0:8680";

/// Choose one of `-a <kaonic-grpc-address>` or
/// `-p <udp-listen-port> -f <udp-forward-address>`
#[derive(Parser)]
#[clap(name = "Rns-Mavlink Flight Controller Bridge", version)]
pub struct Command {
  #[clap(short = 'a', long, group = "transport",
    required_unless_present = "udp_listen_port",
    help = "Reticulum kaonic-ctrl server UDP address")]
  pub kaonic_ctrl_server: Option<SocketAddr>,
  #[clap(short = 'l', long, requires = "kaonic_ctrl_server",
    help = "Reticulum kaonic-ctrl listen UDP address")]
  pub kaonic_ctrl_listen: Option<SocketAddr>,
  #[clap(short = 'p', long, group = "transport",
    required_unless_present = "kaonic_ctrl_server",
    help = "Reticulum UDP listen port")]
  pub udp_listen_port: Option<u16>,
  #[clap(short = 'f', long, requires = "udp_listen_port",
    help = "Reticulum UDP forward address")]
  pub udp_forward_address: Option<SocketAddr>,
  #[arg(short, long,
    help = "[Optional] Reticulum private ID from name string, overrides saved seed file")]
  pub id_seed: Option<String>,
  #[arg(long, default_value = DEFAULT_HTTP_BIND,
    help = "HTTP dashboard bind address")]
  pub http_bind: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), process::ExitCode> {
  // parse command line args
  let cmd = Command::parse();
  // load config
  let config: rns_mavlink::fc::Config = {
    use std::io::Read;
    let mut s = String::new();
    let mut f = std::fs::File::open(rns_mavlink::fc::CONFIG_PATH).unwrap();
    assert!(f.read_to_string(&mut s).unwrap() > 0);
    toml::from_str(&s).unwrap()
  };

  // init logging
  env_logger::Builder::from_env(env_logger::Env::default()
    .default_filter_or(&config.log_level)).init();
  log::info!("fc start");

  // launch plugin dashboard
  if rustls::crypto::CryptoProvider::get_default().is_none() {
    let _ = rustls::crypto::ring::default_provider().install_default();
  }
  let dashboard_state = FcAppState {
    config: Arc::new(RwLock::new(config.clone())),
  };
  let http_bind = cmd.http_bind;
  tokio::spawn(async move {
    if let Err(err) = dashboard::start_server(http_bind, dashboard_state).await {
      log::error!("dashboard server error: {err}");
    }
  });

  // start reticulum
  let id = {
    let id_seed = if let Some(id_seed) = cmd.id_seed {
      id_seed
    } else {
      rns_mavlink::load_or_create_id_seed("fc-id-seed.txt").map_err(|err|{
        log::error!("error loading id seed: {err}");
        process::ExitCode::FAILURE
      })?
    };
    PrivateIdentity::new_from_name(&id_seed)
  };
  log::info!("starting reticulum with identity: {}", id.address_hash().to_hex_string());
  let transport = Transport::new(TransportConfig::new("fc", &id, true));
  let destination = SingleInputDestination::new(id,
    DestinationName::new("rns_mavlink", "fc"));
  log::info!("created destination: {}", destination.desc.address_hash);
  let radio_client = if let Some(server_addr) = cmd.kaonic_ctrl_server.as_ref() {
    // kaonic
    let listen_addr = cmd.kaonic_ctrl_listen.as_ref()
      .expect("required cmd parameter should be checked by parser");
    log::info!("creating RNS kaonic interface with kaonic-ctrl listen address \
      {listen_addr} and server address {server_addr}");
    let radio_client = config.radio_config
      .init_kaonic_radio_client(*listen_addr, *server_addr).await
      .map_err(|err|{
        log::error!("error creating kaonic-ctrl radio client: {err:?}");
        process::ExitCode::FAILURE
      })?;
    let _ = transport.iface_manager().lock().await.spawn(
      KaonicCtrlInterface::new(radio_client.clone(), config.radio_config.radio_module,
        None),
      KaonicCtrlInterface::spawn);
    Some(radio_client)
  } else {
    // udp
    let port = cmd.udp_listen_port.unwrap();
    let forward = cmd.udp_forward_address.unwrap();
    log::info!("creating RNS UDP interface with listen \
      port {port} and forward node {forward}");
    let _ = transport.iface_manager().lock().await.spawn(
      UdpInterface::new(format!("0.0.0.0:{}", cmd.udp_listen_port.unwrap()),
        Some(cmd.udp_forward_address.unwrap().to_string())),
      UdpInterface::spawn);
    None
  };
  // mavlink bridge
  let mut fc = match rns_mavlink::Fc::new(config, radio_client) {
    Ok(fc) => fc,
    Err(err) => {
      log::error!("error creating fc bridge: {:?}", err);
      return Err(process::ExitCode::FAILURE)
    }
  };
  // run
  if let Err(err) = fc.run(transport).await {
    log::error!("fc bridge exited with error: {:?}", err);
  } else {
    log::info!("fc exit");
  }
  Ok(())
}
