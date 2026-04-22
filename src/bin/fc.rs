use clap::Parser;
use log;
use kaonic_reticulum::KaonicCtrlInterface;

use reticulum::destination::{DestinationName, SingleInputDestination};
use reticulum::identity::PrivateIdentity;
use reticulum::iface::udp::UdpInterface;
use reticulum::transport::{Transport, TransportConfig};

use rns_mavlink;

/// Choose one of `-a <kaonic-grpc-address>` or
/// `-p <udp-listen-port> -f <udp-forward-address>`
#[derive(Parser)]
#[clap(name = "Rns-Mavlink Flight Controller Bridge", version)]
pub struct Command {
  #[clap(short = 'a', long, group = "transport",
    required_unless_present = "udp_listen_port",
    help = "Reticulum kaonic-ctrl server UDP address")]
  pub kaonic_ctrl_server: Option<std::net::SocketAddr>,
  #[clap(short = 'l', long, requires = "kaonic_ctrl_server",
    help = "Reticulum kaonic-ctrl listen UDP address")]
  pub kaonic_ctrl_listen: Option<std::net::SocketAddr>,
  #[clap(short = 'p', long, group = "transport",
    required_unless_present = "kaonic_ctrl_server",
    help = "Reticulum UDP listen port")]
  pub udp_listen_port: Option<u16>,
  #[clap(short = 'f', long, requires = "udp_listen_port",
    help = "Reticulum UDP forward address")]
  pub udp_forward_address: Option<std::net::SocketAddr>
}

#[tokio::main]
async fn main() {
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
  // start reticulum
  log::info!("starting reticulum");
  let id = PrivateIdentity::new_from_name("mavlink-rns-fc");
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
    let radio_client = match rns_mavlink::init_kaonic_radio_client(
      *listen_addr, *server_addr, config.radio_module, config.radio_config,
      config.radio_modulation
    ).await {
      Ok(radio_client) => radio_client,
      Err(err) => {
        log::error!("error creating kaonic-ctrl radio client: {err:?}");
        std::process::exit(1)
      }
    };
    let _ = transport.iface_manager().lock().await.spawn(
      KaonicCtrlInterface::new(radio_client.clone(), 0, None),
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
      std::process::exit(1)
    }
  };
  // run
  if let Err(err) = fc.run(transport).await {
    log::error!("fc bridge exited with error: {:?}", err);
  } else {
    log::info!("fc exit");
  }
}
