use std::net;
use std::sync::Arc;

use mavlink;
use radio_common::{Modulation, RadioConfig};
use radio_common::modulation::OfdmModulation;
use serde::Deserialize;
use tokio;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time;

use reticulum::destination::SingleInputDestination;
use reticulum::destination::link::{LinkEvent, LinkId, LinkStatus};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

use crate::{SharedRadioClient, Throughput, THROUGHPUT_LOG_FREQUENCY_SECONDS};

pub struct Gc {
  config: Config,
  radio_client: Option<SharedRadioClient>
}

const fn default_announce_interval_seconds() -> u64 { 5 }

fn default_radio_modulation() -> Modulation {
  Modulation::Ofdm(OfdmModulation::default())
}

#[derive(Deserialize)]
pub struct Config {
  pub log_level: String,
  #[serde(default)]
  pub log_throughput: bool,
  #[serde(default)]
  pub gc_udp_address: Option<net::Ipv4Addr>,
  pub gc_udp_subnet: net::Ipv4Addr,
  pub gc_udp_port: u16,
  pub gc_reply_port: u16,
  #[serde(default="default_announce_interval_seconds")]
  pub announce_interval_seconds: u64,
  // TODO: deserialize AddressHash
  pub fc_destination: String,
  pub radio_module: usize,
  pub radio_config: RadioConfig,
  #[serde(default="default_radio_modulation")]
  pub radio_modulation: Modulation,
  /// Whether to announce radio config link
  #[serde(default)]
  pub radio_config_link: bool
}

#[derive(Debug)]
pub enum Error {
  IoError(std::io::Error)
}

fn heartbeat() -> Result<Vec<u8>, mavlink::error::MessageWriteError>  {
    use std::sync::atomic::{AtomicU8, Ordering};
    use mavlink::MavHeader;
    use mavlink::common::{
      HEARTBEAT_DATA,
      MavAutopilot,
      MavMessage,
      MavType,
      MavModeFlag,
      MavState,
    };
    static SEQNUM: AtomicU8 = AtomicU8::new(0);
    let sequence = SEQNUM.load(Ordering::SeqCst).wrapping_add(1);
    SEQNUM.store(sequence, Ordering::SeqCst);
    let header = MavHeader {
        system_id: 1,     // your system ID (1 is typical for a vehicle)
        component_id: 1,  // autopilot component
        sequence
    };

    let heartbeat = MavMessage::HEARTBEAT(HEARTBEAT_DATA {
        custom_mode: 0,
        mavtype: MavType::MAV_TYPE_QUADROTOR, // or MAV_TYPE_GENERIC, etc.
        autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
        base_mode: MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED |
          MavModeFlag::MAV_MODE_FLAG_STABILIZE_ENABLED |
          MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED,
        system_status: MavState::MAV_STATE_STANDBY,
        mavlink_version: 3, // always 3 for MAVLink v2
    });

    let mut out = vec![];
    let _ = mavlink::write_v1_msg::<mavlink::common::MavMessage, _>(
      &mut out, header, &heartbeat)?;
    Ok(out)
}

impl Gc {
  pub fn new(config: Config, radio_client: Option<SharedRadioClient>) -> Self {
    Gc { config, radio_client }
  }

  pub async fn run(&self,
    transport: Transport,
    data_destination: Arc<Mutex<SingleInputDestination>>
  ) -> Result<(), Error> {
    log::info!("running gc");
    let data_destination_hash = data_destination.lock().await.desc.address_hash;
    let throughput = Arc::new(Mutex::new(Throughput::new_gc()));
    // ping radio client
    let ping_radio_client_loop = async || {
      loop {
        if let Some(radio_client) = self.radio_client.as_ref() {
          match radio_client.lock().await.ping().await {
            Ok(()) => log::trace!("kaonic radio client ping ok"),
            Err(err) => {
              log::error!("kaonic radio client ping error: {err:?}");
              break
            }
          }
        }
        time::sleep(time::Duration::from_secs(10)).await
      }
    };
    // send announces
    let data_link_id: Arc<Mutex<Option<LinkId>>> = Arc::new(Mutex::new(None));
    let announce_loop = async || loop {
      log::debug!("sending announce");
      transport.send_announce(&data_destination, None).await;
      time::sleep(time::Duration::from_secs(self.config.announce_interval_seconds))
        .await;
      if let Some(link_id) = data_link_id.lock().await.as_ref() {
        if let Some(link) = transport.find_in_link(link_id).await {
          let status = link.lock().await.status();
          match status {
            // the link_id should only be set by the LinkActivated event; the link
            // should never become pending or handshake after this event
            LinkStatus::Closed |  LinkStatus::Pending | LinkStatus::Handshake => {
              log::warn!("data link is not open {status:?}, discarding link");
              *data_link_id.lock().await = None;
            }
            _ => {}
          }
        } else {
          log::error!("could not find link {link_id}");
        }
      }
    };
    // listen for packets from ground control station
    let ground_station_address = Arc::new(Mutex::new(None));
    log::info!("creating ground control UDP reply socket for port {}",
      self.config.gc_reply_port);
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", self.config.gc_reply_port)).await
      .map_err(Error::IoError)?;
    let socket_loop = async || {
      let ground_station_timeout = time::Duration::from_secs(10);
      loop {
        // use configured ground station address if present
        if let Some(addr) = self.config.gc_udp_address.as_ref() {
          let socket_addr = net::SocketAddrV4::new(*addr, self.config.gc_udp_port).into();
          log::info!("using configured ground station address: {socket_addr}");
          let bytes = match heartbeat() {
            Ok(bytes) => bytes,
            Err(err) => {
              log::error!("error creating heartbeat packet: {err}");
              return
            }
          };
          match socket.send_to(&bytes, socket_addr).await {
            Ok(_) => {}
            Err(err) => if err.kind() == std::io::ErrorKind::NetworkUnreachable {
              // network may not yet be reachable
              log::warn!("network unreachable");
              time::sleep(time::Duration::from_secs(4)).await;
              continue
            } else {
              log::error!("error sending message to ground station: {err}");
              return
            }
          }
          *ground_station_address.lock().await = Some(socket_addr);
        } else {
          // search for ground station on network at gc_udp_port
          // TODO: would be nice to use mdns here but mdns crate didn't seem to work
          let mut n = 2;  // iterate over addresses
          let mut buf = vec![0; 64];
          let mut t_search = time::Instant::now() - time::Duration::from_secs (5);
          loop {
            let subnet = self.config.gc_udp_subnet.octets();
            let now = time::Instant::now();
            if now - t_search >= time::Duration::from_secs (5) {
              log::info!("searching for ground station on subnet {}.{}.{}.0/24 at port {}",
                subnet[0], subnet[1], subnet[2], self.config.gc_udp_port);
              t_search = now;
            }
            let address = net::SocketAddrV4::new(
              net::Ipv4Addr::new(subnet[0], subnet[1], subnet[2], n), self.config.gc_udp_port);
            let bytes = match heartbeat() {
              Ok(bytes) => bytes,
              Err(err) => {
                log::error!("error creating heartbeat packet: {err}");
                return
              }
            };
            match socket.send_to(&bytes, address).await {
              Ok(_) => {}
              Err(err) => if err.kind() == std::io::ErrorKind::NetworkUnreachable {
                // network may not yet be reachable
                if t_search == now {
                  log::warn!("network unreachable");
                }
              } else {
                log::error!("error searching for ground station: {err}");
                return
              }
            }
            if let Ok(Ok((_, peer))) = tokio::time::timeout(
              time::Duration::from_millis(50), socket.recv_from(&mut buf[..])
            ).await {
              log::info!("got ground station reply from {peer}");
              *ground_station_address.lock().await = Some(peer);
              break
            }
            n += 1;
            if n == 255 {
              n = 2;
            }
          }
        }
        log::info!("listening for UDP packets on port {}",
          socket.local_addr().unwrap().port());
        let mut buf = vec![0u8; 2usize.pow(16)];
        loop {
          // read socket
          match time::timeout(ground_station_timeout, socket.recv_from(&mut buf)).await {
            Ok(Ok((size, src))) => {
              if size == 0 {
                log::warn!("zero size UDP packet data");
                continue
              }
              throughput.lock().await.ground_station_bytes(size as u64);
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
                      if self.config.log_throughput {
                        throughput.lock().await.send_packet(data.len() as u32);
                      }
                    }
                    Err(err) => log::error!("error creating data packet: {err:?}")
                  }
                } else {
                  log::error!("could not find data link ({link_id})")
                }
              }
            }
            Ok(Err(e)) => log::error!("error receiving packet: {e}"),
            Err(_) => {
              log::warn!("ground station socket no data received in \
                {ground_station_timeout:?}, assumed disconnected");
              let _ = ground_station_address.lock().await.take();
              break
            }
          }
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
                  if let Some(target) = ground_station_address.lock().await.clone() {
                    log::trace!("send payload ({}) to {target}", payload.len());
                    match socket.send_to(&payload.as_slice(), target).await {
                      Ok(n) => log::trace!("socket sent {n} bytes"),
                      Err(err) => {
                        log::error!("socket error sending bytes: {err:?}");
                        break
                      }
                    }
                    if self.config.log_throughput {
                      throughput.lock().await.recv_packet(payload.len() as u32);
                    }
                  } else {
                    log::trace!("dropping payload: no ground station address");
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
    // throughput log
    let throughput_log_loop = async || {
      if self.config.log_throughput {
        loop {
          time::sleep(time::Duration::from_secs(THROUGHPUT_LOG_FREQUENCY_SECONDS)).await;
          throughput.lock().await.log();
        }
      } else {
        std::future::pending::<()>().await;
      }
    };
    // run
    tokio::select!{
      _ = throughput_log_loop() =>
        log::info!("throughput log loop exited: shutting down"),
      _ = ping_radio_client_loop() =>
        log::info!("ping radio client loop exited: shutting down"),
      _ = announce_loop() => log::info!("announce loop exited: shutting down"),
      _ = socket_loop() => log::info!("socket loop exited: shutting down"),
      _ = link_event_loop() => log::info!("link event loop exited: shutting down"),
      _ = tokio::signal::ctrl_c() => log::info!("got ctrl-c: shutting down")
    }
    Ok(())
  }
}
