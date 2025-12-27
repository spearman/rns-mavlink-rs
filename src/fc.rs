use std::ops::RangeInclusive;
use std::sync::Arc;

use range_set::RangeSet;
use serde::{Deserialize, Serialize};
use tokio;
use tokio::time;
use tokio::sync::mpsc;

use reticulum::destination::link::{Link, LinkEvent, LinkStatus};
use reticulum::iface::kaonic::{self, RadioConfig};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

use crate::{MavlinkBuffer, Seqnum};

pub const CONFIG_PATH: &str = "Fc.toml";

pub struct Fc {
  config: Config,
  radio_config_tx: Option<mpsc::Sender<kaonic::RadioConfig>>,
  mavlink_buffer: Arc<tokio::sync::Mutex<MavlinkBuffer>>,
  received: Arc<tokio::sync::Mutex<RangeSet<[RangeInclusive<u64>; 4]>>>
}

#[derive(Deserialize, Serialize)]
pub struct Config {
  pub log_level: String,
  pub serial_port: String,
  pub serial_baud: u32,
  // TODO: deserialize AddressHash
  pub gc_data_destination: String,
  // TODO: deserialize AddressHash
  pub gc_radio_config_destination: String,
  /// This will be overwritten if Gc changes config
  pub radio_config: RadioConfig
}

#[derive(Debug)]
pub enum Error {
  SerialDeviceError(tokio_serial::Error),
  RnsError(reticulum::error::RnsError)
}

impl Fc {
  pub fn new(config: Config, radio_config_tx: Option<mpsc::Sender<kaonic::RadioConfig>>)
    -> Result<Self, ()>
  {
    let mavlink_buffer = Arc::new(tokio::sync::Mutex::new(MavlinkBuffer::new()));
    let received = Arc::new(tokio::sync::Mutex::new(RangeSet::new()));
    let fc = Fc { config, radio_config_tx, mavlink_buffer, received };
    Ok(fc)
  }

  pub async fn run(&mut self, transport: Transport) -> Result<(), Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_serial::SerialPortBuilderExt;
    log::info!("running fc");
    type MaybeLink = Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Link>>>>>;
    let data_link: MaybeLink = Arc::new(tokio::sync::Mutex::new(None));
    let config_link: MaybeLink = Arc::new(tokio::sync::Mutex::new(None));
    let parse_destination_hash = |hash, name|
      AddressHash::new_from_hex_string(hash).map_err(|err|{
        log::error!("error parsing ground control {name} destination hash: {err:?}");
        Error::RnsError(err)
      });
    let gc_data_destination =
      parse_destination_hash(&self.config.gc_data_destination, "data")?;
    let gc_config_destination =
      parse_destination_hash(&self.config.gc_radio_config_destination, "radio config")?;
    log::debug!("gc data destination: {gc_data_destination}");
    log::debug!("gc radio config destination: {gc_config_destination}");
    let serial_port = self.config.serial_port.clone();
    log::info!("opening serial port {serial_port}");
    let port = tokio_serial::new(&serial_port, self.config.serial_baud)
      .open_native_async()
      .map_err(Error::SerialDeviceError)?;
    let (mut port_reader, mut port_writer) = tokio::io::split(port);
    // set up links
    let link_loop = async || {
      let mut announce_recv = transport.recv_announces().await;
      while let Ok(announce) = announce_recv.recv().await {
        let destination = announce.destination.lock().await;
        let address = destination.desc.address_hash;
        if address == gc_data_destination {
          log::debug!("got gc data destination announce {address}");
          let mut data_link = data_link.lock().await;
          if data_link.is_none() {
            log::info!("creating data link for {address}");
            *data_link = Some(transport.link(destination.desc).await);
            self.received.lock().await.clear();
          }
        } else if address == gc_config_destination {
          log::debug!("got gc radio config destination announce {address}");
          let mut config_link = config_link.lock().await;
          if config_link.is_none() {
            log::info!("creating radio config link for {address}");
            *config_link = Some(transport.link(destination.desc).await);
          }
        }
      }
    };
    // read serial port and forward to links
    let mut read_port_loop = async || {
      let mut debug_ts = time::Instant::now() - time::Duration::from_secs(2);
      loop {
        if data_link.lock().await.is_some() {
          log::info!("reading from serial port {}", serial_port);
          let mut buf = vec![0u8; 2usize.pow(16)];
          'read_loop: loop {
            match port_reader.read(&mut buf).await {
              Ok(n) => {
                log::trace!("read {n} bytes");
                for data in buf[..n].chunks(reticulum::packet::PACKET_MDU / 2) {
                  let mut lock = data_link.lock().await;
                  if let Some(link_mutex) = lock.take() {
                    let mut link = link_mutex.lock().await;
                    let (seqnum, payload) = self.mavlink_buffer.lock().await
                      .push(data.to_vec());
                    log::trace!("sending seq[{}] on link ({})", seqnum.0, link.id());
                    match link.data_packet(&payload[..]) {
                      Ok(packet) => {
                        drop(link); // drop before sending to prevent deadlock
                        transport.send_packet(packet).await;
                        *lock = Some(link_mutex);
                      }
                      Err(err) => {
                        log::error!("error creating data packet: {err:?}");
                        link.close();
                        break 'read_loop
                      }
                    }
                  } else {
                    log::info!("data link lost, waiting for link to be re-established");
                    break 'read_loop
                  }
                }
              }
              Err(e) => log::error!("error reading serial port: {}", e)
            }
          }
        }
        let now = time::Instant::now();
        if now - debug_ts >= time::Duration::from_secs(2) {
          log::debug!("read port loop: waiting for data link");
          debug_ts = now;
        }
        time::sleep(time::Duration::from_millis(200)).await;
      }
    };
    // handle link events
    let mut link_event_loop = async || {
      let mut out_link_events = transport.out_link_events();
      while let Ok(link_event) = out_link_events.recv().await {
        log::trace!("got out link event with address hash: {}", link_event.address_hash);
        if link_event.address_hash == gc_data_destination {
          // forward upstream link messages to serial port
          match link_event.event {
            LinkEvent::Data(payload) => {
              log::trace!("data link {} payload ({})", link_event.id, payload.len());
              // parse payload: [seqnum, data]
              let seqnum =
                u64::from_be_bytes(payload.as_slice()[0..8].try_into().unwrap());
              if payload.len() > 8 {
                // TODO: port write and ack reply in parallel?
                // data packet
                if self.received.lock().await.insert(seqnum) {
                  match port_writer.write_all(&payload.as_slice()[8..]).await {
                    Ok(()) => log::trace!("port sent {} bytes", payload.len()),
                    Err(err) => {
                      log::error!("port error sending bytes: {err:?}");
                      break
                    }
                  }
                } else {
                  log::debug!("already received seqnum {seqnum}");
                }
                // send ack
                if let Some(link) = transport.find_in_link(&link_event.id).await {
                  let link = link.lock().await;
                  match link.data_packet(&seqnum.to_be_bytes()[..]) {
                    Ok(packet) => {
                      drop(link); // drop to prevent deadlock
                      transport.send_packet(packet).await;
                    }
                    Err(err) => log::error!("error creating ack packet: {err:?}")
                  }
                } else {
                  log::error!("could not find link for ack")
                }
              } else {
                // ack packet
                self.mavlink_buffer.lock().await.ack(Seqnum(seqnum));
              }
            }
            LinkEvent::Activated => {
              log::info!("data link activated {}", link_event.id);
              debug_assert!(data_link.lock().await.is_some());
            }
            LinkEvent::Closed => {
              log::warn!("data link closed {}", link_event.id);
              let _ = data_link.lock().await.take();
            }
          }
        } else if link_event.address_hash == gc_config_destination {
          // handle radio config messages
          match link_event.event {
            LinkEvent::Data(payload) => {
              log::trace!("radio config link {} payload ({})",
                link_event.id, payload.len());
              match serde_json::from_slice::<RadioConfig>(payload.as_slice()) {
                Ok(radio_config) => {
                  if radio_config == self.config.radio_config {
                    log::info!("got gc radio config: matches current radio config");
                  } else {
                    log::info!("got gc radio config: updating radio config");
                    if let Some(tx) = self.radio_config_tx.as_ref() {
                      self.config.radio_config = radio_config.clone();
                      match tx.send(radio_config).await {
                        Ok(()) => {}
                        Err(err) => {
                          log::error!("error sending radio config: {err}");
                          break
                        }
                      }
                      // write config file
                      let s = toml::to_string(&self.config).unwrap();
                      match std::fs::write(CONFIG_PATH, &s) {
                        Ok(_) => {}
                        Err(err) => {
                          log::error!("error writing config file: {err}");
                          break
                        }
                      }
                      // TODO: ack ?
                      // pause to let config update take effect
                      time::sleep(time::Duration::from_millis(500)).await;
                      // the links will need to be re-created
                      if let Some(link) = data_link.lock().await.take() {
                        log::info!("closing data link");
                        link.lock().await.close();
                      }
                      if let Some(link) = config_link.lock().await.take() {
                        log::info!("closing config link");
                        link.lock().await.close();
                      }
                    } else {
                      log::error!("fc missing radio config tx channel");
                      panic!("fc missing radio config tx channel");
                    }
                  }
                }
                Err(err) => {
                  log::error!("error deserializing radio config message: {err}");
                  break
                }
              };
            }
            LinkEvent::Activated => {
              log::info!("radio config link activated {}", link_event.id);
              debug_assert!(config_link.lock().await.is_some());
            }
            LinkEvent::Closed => {
              log::warn!("radio config link closed {}", link_event.id);
              let _ = config_link.lock().await.take();
            }
          }
        }
      }
    };
    // re-send un-acked messages after a timeout
    let retransmit_loop = async || {
      let default_sleep = time::Duration::from_millis(100);
      let mut debug_ts = time::Instant::now();
      let mut resend_counter = 0;
      loop {
        let lock = data_link.lock().await;
        if let Some(link) = lock.as_ref() {
          let sleep_for = if self.mavlink_buffer.lock().await.buffer().is_empty() {
            // no messages to re-transmit
            default_sleep
          } else {
            let link = link.lock().await;
            let status = link.status();
            if status == LinkStatus::Active {
              // get the RTT to calculate timeout
              let rtt = *link.rtt();
              let timeout = (rtt * 3) / 2;  // * 1.5
              // re-transmit
              let retransmit = self.mavlink_buffer.lock().await.retransmit(timeout);
              let packets = retransmit.into_iter().map(|payload|
                match link.data_packet(payload.as_slice()) {
                  Ok(packet) => packet,
                  Err(err) => {
                    log::error!("error creating re-transmit packet: {err:?}");
                    panic!("error creating re-transmit packet: {err:?}")
                  }
                }
              ).collect::<Vec<_>>();
              drop(link); // drop to prevent deadlock
              drop(lock);
              let npackets = packets.len();
              log::debug!("re-sending {npackets} messages");
              for packet in packets {
                transport.send_packet(packet).await;
              }
              resend_counter += npackets;
              rtt / 2
            } else {
              if status == LinkStatus::Closed {
                log::info!("data link is closed");
                drop(link);
                let _ = data_link.lock().await.take();
              } else {
                log::info!("data link is not yet active")
              }
              default_sleep
            }
          };
          time::sleep(sleep_for).await;
        } else {
          drop(lock);
          time::sleep(default_sleep).await;
        }
        if debug_ts.elapsed() >= time::Duration::from_secs(2) {
          log::debug!(
            "retransmit loop: (current seqnum, num resends, received count): \
              ({}, {resend_counter}, {})",
            self.mavlink_buffer.lock().await.sequence.0,
            self.received.lock().await.len());
          debug_ts = time::Instant::now();
        }
      }
    };
    // run
    tokio::select!{
      _ = read_port_loop() => log::info!("read port loop exited: shutting down"),
      _ = link_event_loop() => log::info!("link event loop exited: shutting down"),
      _ = link_loop() => log::info!("link loop exited: shutting down"),
      _ = retransmit_loop() => log::info!("retransmit loop exited: shutting down"),
      _ = tokio::signal::ctrl_c() => log::info!("got ctrl-c: shutting down")
    }
    Ok(())
  }
}
