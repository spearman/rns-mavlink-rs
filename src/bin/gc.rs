use std::str;

use clap::Parser;
use kaonic_reticulum::KaonicCtrlInterface;
use log;
use toml;

use reticulum::destination::DestinationName;
use reticulum::identity::PrivateIdentity;
use reticulum::iface::udp::UdpInterface;
use reticulum::transport::{Transport, TransportConfig};

use rns_mavlink;

const CONFIG_PATH: &str = "Gc.toml";

/// Choose one of `-a <kaonic-grpc-address>` or
/// `-p <udp-listen-port> -f <udp-forward-address>`
#[derive(Parser)]
#[clap(name = "Rns-Mavlink Ground Control Bridge", version)]
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
  let config: rns_mavlink::gc::Config = {
    use std::io::Read;
    let mut s = String::new();
    let mut f = std::fs::File::open(CONFIG_PATH).unwrap();
    assert!(f.read_to_string(&mut s).unwrap() > 0);
    toml::from_str(&s).unwrap()
  };
  // init logging
  env_logger::Builder::from_env(env_logger::Env::default()
    .default_filter_or(&config.log_level)).init();
  log::info!("gc start");
  // start reticulum
  log::info!("starting reticulum");
  let id = PrivateIdentity::new_from_name("mavlink-rns-gc");
  let mut transport = Transport::new(TransportConfig::new("gc", &id, true));
  // create destinations
  let data_destination = transport.add_destination(id.clone(),
    DestinationName::new("rns_mavlink", "gc.mavlink_data")).await;
  log::info!("created data destination: {}",
    data_destination.lock().await.desc.address_hash);
  let config_destination = if let Some(server_addr) =
    cmd.kaonic_ctrl_server.as_ref()
  {
    // kaonic
    let listen_addr = cmd.kaonic_ctrl_listen.as_ref()
      .expect("required cmd parameter should be checked by parser");
    let config_destination = transport.add_destination(id.clone(),
      DestinationName::new("rns_mavlink", "gc.radio_config")).await;
    log::info!("created radio config destination: {}",
      config_destination.lock().await.desc.address_hash);

    log::info!("creating RNS kaonic interface with kaonic-ctrl server address {}",
      server_addr);
    let radio_client = match rns_mavlink::init_kaonic_radio_client(
      *listen_addr, *server_addr, config.radio_module, config.radio_config
    ).await {
      Ok(radio_client) => radio_client,
      Err(err) => {
        log::error!("error creating kaonic-ctrl radio client: {err:?}");
        std::process::exit(1)
      }
    };
    let _ = transport.iface_manager().lock().await.spawn(
      KaonicCtrlInterface::new(radio_client.clone(), 0),
      KaonicCtrlInterface::spawn);
    Some(config_destination)
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
  let gc = rns_mavlink::Gc::new(config);
  if let Err(err) = gc.run(transport, data_destination, config_destination).await {
    log::error!("gc bridge exited with error: {:?}", err);
  } else {
    log::info!("gc exit");
  }
}
