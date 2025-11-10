use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio;
use tokio::time;
use tokio::sync::mpsc;

use reticulum::destination::link::{Link, LinkEvent};
use reticulum::iface::kaonic::{self, RadioConfig};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

pub const CONFIG_PATH: &str = "Fc.toml";

pub struct Fc {
  config: Config,
  radio_config_tx: mpsc::Sender<kaonic::RadioConfig>
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
  pub fn new(config: Config, radio_config_tx: mpsc::Sender<kaonic::RadioConfig>)
    -> Result<Self, ()>
  {
    let fc = Fc { config, radio_config_tx };
    Ok(fc)
  }

  pub async fn run(&mut self, transport: Transport) -> Result<(), Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_serial::SerialPortBuilderExt;
    let data_link: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Link>>>>> =
      Arc::new(tokio::sync::Mutex::new(None));
    let config_link: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Link>>>>> =
      Arc::new(tokio::sync::Mutex::new(None));
    let gc_data_destination = AddressHash::new_from_hex_string(&self.config.gc_data_destination)
      .map_err(|err|{
        log::error!("error parsing ground control destination hash: {err:?}");
        Error::RnsError(err)
      })?;
    let gc_config_destination = AddressHash::new_from_hex_string(
      &self.config.gc_radio_config_destination
    ) .map_err(|err|{
        log::error!("error parsing ground control destination hash: {err:?}");
        Error::RnsError(err)
      })?;
    let serial_port = self.config.serial_port.clone();
    let port = tokio_serial::new(&serial_port, self.config.serial_baud)
      .open_native_async()
      .map_err(Error::SerialDeviceError)?;
    let (mut port_reader, mut port_writer) = tokio::io::split(port);
    // set up links
    let link_loop = async || {
      let mut announce_recv = transport.recv_announces().await;
      // TODO: continue looping after link is created?
      while let Ok(announce) = announce_recv.recv().await {
        let destination = announce.destination.lock().await;
        let address = destination.desc.address_hash;
        if address == gc_data_destination {
          let mut data_link = data_link.lock().await;
          if data_link.is_none() {
            *data_link = Some(transport.link(destination.desc).await);
          }
        } else if address == gc_config_destination {
          let mut config_link = config_link.lock().await;
          if config_link.is_none() {
            *config_link = Some(transport.link(destination.desc).await);
          }
        }
      }
    };
    // read serial port and forward to links
    let mut read_port_loop = async || {
      loop {
        if let Some(_link) = data_link.lock().await.as_ref() {
          log::info!("reading from serial port {}...", serial_port);
          let mut buf = vec![0u8; 2usize.pow(16)];
          loop {
            match port_reader.read(&mut buf).await {
              Ok(n) => {
                log::trace!("read {n} bytes");

                for data in buf[..n].chunks(reticulum::packet::PACKET_MDU / 2) {
                  transport.send_to_all_out_links(data).await;
                }
              }
              Err(e) => log::error!("error reading serial port: {}", e)
            }
          }
        }
        time::sleep(time::Duration::from_millis(200)).await;
      }
    };
    // handle link events
    let mut link_event_loop = async || {
      let mut out_link_events = transport.out_link_events();
      while let Ok(link_event) = out_link_events.recv().await {
        if link_event.address_hash == gc_data_destination {
          // forward upstream link messages to serial port
          match link_event.event {
            LinkEvent::Data(payload) => {
              log::trace!("data link {} payload ({})", link_event.id, payload.len());
              match port_writer.write_all(payload.as_slice()).await {
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
          }
        } else if link_event.address_hash == gc_config_destination {
          // handle radio config messages
          match link_event.event {
            LinkEvent::Data(payload) => {
              log::trace!("config link {} payload ({})", link_event.id, payload.len());
              match serde_json::from_slice::<RadioConfig>(payload.as_slice()) {
                Ok(radio_config) => {
                  if radio_config == self.config.radio_config {
                    log::info!("got gc radio config: matches current radio config");
                  } else {
                    log::info!("got gc radio config: updating radio config");
                    self.config.radio_config = radio_config.clone();
                    match self.radio_config_tx.send(radio_config).await {
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
                  }
                }
                Err(err) => {
                  log::error!("error deserializing radio config message: {err}");
                  break
                }
              };
            }
            LinkEvent::Activated => {
              log::info!("config link activated {}", link_event.id);
              debug_assert!(config_link.lock().await.is_some());
            }
            LinkEvent::Closed => {
              log::warn!("config link closed {}", link_event.id);
              let _ = config_link.lock().await.take();
            }
          }
        }
      }
    };
    // run
    tokio::select!{
      _ = read_port_loop() => log::info!("read port loop exited: shutting down"),
      _ = link_event_loop() => log::info!("link event loop exited: shutting down"),
      _ = link_loop() => log::info!("link loop exited: shutting down"),
      _ = tokio::signal::ctrl_c() => log::info!("got ctrl-c: shutting down")
    }
    Ok(())
  }
}
