use std::sync::Arc;

use radio_common::RadioConfig;
use serde::{Deserialize, Serialize};
use tokio;
use tokio::time;

use reticulum::destination::link::{Link, LinkEvent, LinkStatus};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

use crate::SharedRadioClient;

pub const CONFIG_PATH: &str = "Fc.toml";

pub struct Fc {
  config: Config,
  radio_client: Option<SharedRadioClient>
}

#[derive(Deserialize, Serialize)]
pub struct Config {
  pub log_level: String,
  pub serial_port: String,
  pub serial_baud: u32,
  // TODO: deserialize AddressHash
  pub gc_data_destination: String,
  pub radio_module: usize,
  pub radio_config: RadioConfig
}

#[derive(Debug)]
pub enum Error {
  SerialDeviceError(tokio_serial::Error),
  RnsError(reticulum::error::RnsError)
}

impl Fc {
  pub fn new(config: Config, radio_client: Option<SharedRadioClient>)
    -> Result<Self, ()>
  {
    let fc = Fc { config, radio_client };
    Ok(fc)
  }

  pub async fn run(&mut self, transport: Transport) -> Result<(), Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_serial::SerialPortBuilderExt;
    log::info!("running fc");
    type MaybeLink = Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Link>>>>>;
    let data_link: MaybeLink = Arc::new(tokio::sync::Mutex::new(None));
    let parse_destination_hash = |hash, name|
      AddressHash::new_from_hex_string(hash).map_err(|err|{
        log::error!("error parsing ground control {name} destination hash: {err:?}");
        Error::RnsError(err)
      });
    let gc_data_destination =
      parse_destination_hash(&self.config.gc_data_destination, "data")?;
    log::debug!("gc data destination: {gc_data_destination}");
    let serial_port = self.config.serial_port.clone();
    log::info!("opening serial port {serial_port}");
    let port = tokio_serial::new(&serial_port, self.config.serial_baud)
      .open_native_async()
      .map_err(Error::SerialDeviceError)?;
    let (mut port_reader, mut port_writer) = tokio::io::split(port);
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
          }
        } else {
          log::debug!("got announce: {address}");
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
                    let link = link_mutex.lock().await;
                    let status = link.status();
                    if status == LinkStatus::Closed {
                      log::info!("read port loop: data link is closed");
                      break 'read_loop
                    } else if status != LinkStatus::Active {
                      drop(link);
                      *lock = Some(link_mutex);
                      log::info!("read port loop: data link is not yet active ({status:?})");
                    } else {
                      log::trace!("sending on link ({})", link.id());
                      match link.data_packet(data) {
                        Ok(packet) => {
                          drop(link); // drop before sending to prevent deadlock
                          transport.send_packet(packet).await;
                          *lock = Some(link_mutex);
                        }
                        Err(err) => {
                          log::error!("error creating data packet: {err:?}");
                          let _ = transport.link_close(*link.id()).await.map_err(|err|
                            log::warn!("error closing data link: {err:?}"));
                          break 'read_loop
                        }
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
      loop {
        match out_link_events.recv().await {
          Ok(link_event) => {
            log::trace!("got out link event with address hash: {}", link_event.address_hash);
            if link_event.address_hash == gc_data_destination {
              // forward upstream link messages to serial port
              match link_event.event {
                LinkEvent::Data(payload) => {
                  log::trace!("data link {} payload ({})", link_event.id, payload.len());
                  // data packet
                  match port_writer.write_all(&payload.as_slice()).await {
                    Ok(()) => log::trace!("port sent {} bytes", payload.len()),
                    Err(err) => {
                      log::error!("port error sending bytes: {err:?}");
                      break
                    }
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
                LinkEvent::Proof(_) => {}
              }
            }
          }
          Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
            log::debug!("recv out link event lagged: {n}");
          }
          Err(err) => {
            log::error!("recv out link event error: {err:?}");
            break
          }
        }
      }
    };
    // run
    tokio::select!{
      _ = ping_radio_client_loop() =>
        log::info!("ping radio client loop exited: shutting down"),
      _ = read_port_loop() => log::info!("read port loop exited: shutting down"),
      _ = link_event_loop() => log::info!("link event loop exited: shutting down"),
      _ = link_loop() => log::info!("link loop exited: shutting down"),
      _ = tokio::signal::ctrl_c() => log::info!("got ctrl-c: shutting down")
    }
    Ok(())
  }
}
