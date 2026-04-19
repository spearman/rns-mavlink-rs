use std::net;
use std::sync::Arc;

use radio_common::RadioConfig;
use serde::Deserialize;
use tokio;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time;

use reticulum::destination::SingleInputDestination;
use reticulum::destination::link::{LinkEvent, LinkId};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

pub struct Gc {
  config: Config
}

#[derive(Deserialize)]
pub struct Config {
  pub log_level: String,
  pub gc_udp_subnet: net::Ipv4Addr,
  pub gc_udp_port: u16,
  pub gc_reply_port: u16,
  // TODO: deserialize AddressHash
  pub fc_destination: String,
  pub radio_module: usize,
  pub radio_config: RadioConfig,
  /// Whether to announce radio config link
  #[serde(default)]
  pub radio_config_link: bool
}

#[derive(Debug)]
pub enum Error {
  IoError(std::io::Error)
}

impl Gc {
  pub fn new(config: Config) -> Self {
    Gc { config }
  }

  pub async fn run(&self,
    transport: Transport,
    data_destination: Arc<Mutex<SingleInputDestination>>,
    kaonic_config_destination: Option<Arc<Mutex<SingleInputDestination>>>
  ) -> Result<(), Error> {
    log::info!("running gc");
    let data_destination_hash = data_destination.lock().await.desc.address_hash;
    let config_destination_hash = match kaonic_config_destination.as_ref() {
      Some(config_destination) =>
        Some(config_destination.lock().await.desc.address_hash),
      None => None
    };
    // send announces
    let announce_loop = async || loop {
      log::debug!("sending announce");
      transport.send_announce(&data_destination, None).await;
      if let Some(config_destination) = kaonic_config_destination.as_ref() {
        transport.send_announce(&config_destination, None).await;
      }
      time::sleep(time::Duration::from_secs(2)).await;
    };
    // link variables
    let data_link_id: Arc<Mutex<Option<LinkId>>> = Arc::new(Mutex::new(None));
    let config_link_id: Arc<Mutex<Option<LinkId>>> = Arc::new(Mutex::new(None));
    // search for ground station on network at gc_udp_port
    // TODO: would be nice to use mdns here but mdns crate didn't seem to work
    // TODO: after the ground station is found the address will be set until the service
    // restarts, can we detect disconnection and change of address ?
    log::info!("creating ground control UDP reply socket for port {}",
      self.config.gc_reply_port);
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", self.config.gc_reply_port))
      .await.map_err(Error::IoError)?;
    let mut n = 2;
    let mut buf = vec![0; 64];
    let mut t_search = time::Instant::now() - time::Duration::from_secs (5);
    let ground_station_address = loop {
      let subnet = self.config.gc_udp_subnet.octets();
      let now = time::Instant::now();
      if now - t_search >= time::Duration::from_secs (5) {
        log::info!("searching for ground station on subnet {}.{}.{}.0/24 at port {}",
          subnet[0], subnet[1], subnet[2], self.config.gc_udp_port);
        t_search = now;
      }
      let address = net::SocketAddrV4::new(
        net::Ipv4Addr::new(subnet[0], subnet[1], subnet[2], n), self.config.gc_udp_port);
      match socket.send_to(b"", address).await {
        Ok(_) => {}
        Err(err) => if err.kind() == std::io::ErrorKind::NetworkUnreachable {
          // network may not yet be reachable
          if t_search == now {
            log::warn!("network unreachable");
          }
        } else {
          return Err(Error::IoError(err))
        }
      }
      if let Ok(Ok((_, peer))) = tokio::time::timeout(
        time::Duration::from_millis(100), socket.recv_from(&mut buf[..])
      ).await {
        log::info!("got ground station reply from {peer}");
        break peer
      }
      n += 1;
      if n == 255 {
        n = 2;
      }
    };
    // listen for packets from ground control station
    let socket_loop = async || {
      log::info!("listening for UDP packets on port {}",
        socket.local_addr().unwrap().port());
      let mut buf = vec![0u8; 1024];
      loop {
        match socket.recv_from(&mut buf).await {
          Ok((size, src)) => {
            if size == 0 {
              log::warn!("zero size UDP packet data");
              continue
            }
            let data = &buf[..size];
            match str::from_utf8(data) {
              Ok(text) => log::trace!("received from {}: {}", src, text),
              Err(_) => log::trace!("received non-UTF8 data from {}: {:?}", src, data),
            }
            let link_id = data_link_id.lock().await;
            if let Some(link_id) = link_id.as_ref() {
              if let Some(link) = transport.find_in_link(link_id).await {
                // if the link is active, forward to flight controller
                let link = link.lock().await;
                log::trace!("sending data on link ({link_id})");
                match link.data_packet(data) {
                  Ok(packet) => {
                    drop(link); // drop to prevent deadlock
                    transport.send_packet(packet).await;
                  }
                  Err(err) => log::error!("error creating data packet: {err:?}")
                }
              } else {
                log::error!("could not find data link ({link_id})")
              }
            }
          }
          Err(e) => log::error!("error receiving packet: {e}")
        }
      }
    };
    // receive upstream link data
    let link_event_loop = async || {
      let _fc_destination =
        match AddressHash::new_from_hex_string(&self.config.fc_destination) {
          Ok(dest) => dest,
          Err(err) => {
            log::error!("error parsing fc destination hash: {err:?}");
            return
          }
        };
      let mut in_link_events = transport.in_link_events();
      let target = ground_station_address;
      loop {
        match in_link_events.recv().await {
          Ok(link_event) => {
            log::trace!("got in link event with address hash: {}",
              link_event.address_hash);
            if link_event.address_hash == data_destination_hash {
              // data link event
              match link_event.event {
                LinkEvent::Data(payload) => {
                  log::trace!("data link {} payload ({})", link_event.id, payload.len());
                  match socket.send_to(&payload.as_slice(), target).await {
                    Ok(n) => log::trace!("socket sent {n} bytes"),
                    Err(err) => {
                      log::error!("socket error sending bytes: {err:?}");
                      break
                    }
                  }
                }
                LinkEvent::Activated => {
                  log::info!("data link activated {}", link_event.id);
                  let mut link_id = data_link_id.lock().await;
                  *link_id = Some(link_event.id);
                }
                LinkEvent::Closed => {
                  log::info!("data link closed {}", link_event.id);
                  let _ = data_link_id.lock().await.take();
                }
                LinkEvent::Proof(_) => {}
              }
            } else if Some(link_event.address_hash) == config_destination_hash {
              // config link event
              match link_event.event {
                LinkEvent::Data(payload) => {
                  log::trace!("config link {} payload ({})", link_event.id, payload.len());
                  // current implementation does not expect to receive data on this link
                  log::error!("TODO: handle config link replies");
                  unimplemented!("TODO: handle config link replies")
                }
                LinkEvent::Activated => {
                  log::info!("config link activated {}", link_event.id);
                  let mut config_link_id = config_link_id.lock().await;
                  *config_link_id = Some(link_event.id);
                  // send config
                  let config = serde_json::to_vec(&self.config.radio_config).unwrap();
                  if let Some(link) = transport.find_in_link(&link_event.id).await {
                    let link = link.lock().await;
                    match link.data_packet(config.as_slice()) {
                      Ok(packet) => {
                        drop(link); // drop to prevent deadlock
                        log::info!("sending radio config");
                        transport.send_packet(packet).await;
                      }
                      Err(err) => log::error!("error creating config packet: {err:?}")
                    }
                  } else {
                    log::error!("could not find config link ({})", link_event.id)
                  }
                }
                LinkEvent::Closed => {
                  log::info!("config link closed {}", link_event.id);
                  let _ = config_link_id.lock().await.take();
                }
                LinkEvent::Proof(_) => {}
              }
            }
          }
          Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            log::debug!("recv in link event lagged: {n}");
          }
          Err(err) => {
            log::error!("recv in link event error: {err:?}");
            break
          }
        }
      }
    };
    tokio::select!{
      _ = announce_loop() => log::info!("announce loop exited: shutting down"),
      _ = socket_loop() => log::info!("socket loop exited: shutting down"),
      _ = link_event_loop() => log::info!("link event loop exited: shutting down"),
      _ = tokio::signal::ctrl_c() => log::info!("got ctrl-c: shutting down")
    }
    Ok(())
  }
}
