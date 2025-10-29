use std::sync::Arc;

use serde::Deserialize;
use tokio;
use tokio::time;
use tokio::sync::mpsc;

use reticulum::destination::link::{Link, LinkEvent};
use reticulum::iface::kaonic::{self, RadioConfig};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

pub struct Fc {
  config: FcConfig,
  radio_config_tx: mpsc::Sender<kaonic::RadioConfig>
}

#[derive(Deserialize)]
pub struct FcConfig {
  pub log_level: String,
  pub serial_port: String,
  pub serial_baud: u32,
  // TODO: deserialize AddressHash
  pub gc_destination: String,
  // TODO: deserialize AddressHash
  pub gc_radio_config_destination: String
}

#[derive(Debug)]
pub enum FcError {
  SerialDeviceError(tokio_serial::Error),
  RnsError(reticulum::error::RnsError)
}

impl Fc {
  pub fn new(config: FcConfig, radio_config_tx: mpsc::Sender<kaonic::RadioConfig>)
    -> Result<Self, ()>
  {
    let fc = Fc { config, radio_config_tx };
    Ok(fc)
  }

  pub async fn run(&self, transport: Transport) -> Result<(), FcError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_serial::SerialPortBuilderExt;
    let link: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Link>>>>> =
      Arc::new(tokio::sync::Mutex::new(None));
    let config_link: Arc<tokio::sync::Mutex<Option<Arc<tokio::sync::Mutex<Link>>>>> =
      Arc::new(tokio::sync::Mutex::new(None));
    let gc_destination = AddressHash::new_from_hex_string(&self.config.gc_destination)
      .map_err(|err|{
        log::error!("error parsing ground control destination hash: {err:?}");
        FcError::RnsError(err)
      })?;
    let gc_config_destination = AddressHash::new_from_hex_string(
      &self.config.gc_radio_config_destination
    ) .map_err(|err|{
        log::error!("error parsing ground control destination hash: {err:?}");
        FcError::RnsError(err)
      })?;
    let port = tokio_serial::new(&self.config.serial_port, self.config.serial_baud)
      .open_native_async()
      .map_err(FcError::SerialDeviceError)?;
    let (mut port_reader, mut port_writer) = tokio::io::split(port);
    // set up links
    let link_loop = async || {
      let mut announce_recv = transport.recv_announces().await;
      // TODO: continue looping after link is created?
      while let Ok(announce) = announce_recv.recv().await {
        let destination = announce.destination.lock().await;
        let address = destination.desc.address_hash;
        if address == gc_destination {
          *link.lock().await = Some(transport.link(destination.desc).await);
        } else if address == gc_config_destination {
          *config_link.lock().await = Some(transport.link(destination.desc).await);
        }
      }
    };
    // read serial port and forward to links
    let mut read_port_loop = async || {
      loop {
        if let Some(_link) = link.lock().await.as_ref() {
          log::info!("reading from serial port {}...", self.config.serial_port);
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
        if link_event.address_hash == gc_destination {
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
            }
            LinkEvent::Closed => {
              log::warn!("data link closed {}", link_event.id);
              let _ = link.lock().await.take();
            }
          }
        } else if link_event.address_hash == gc_config_destination {
          // handle radio config messages
          match link_event.event {
            LinkEvent::Data(payload) => {
              log::trace!("config link {} payload ({})", link_event.id, payload.len());
              match serde_json::from_slice::<RadioConfig>(payload.as_slice()) {
                Ok(radio_config) => {
                  match self.radio_config_tx.send(radio_config).await {
                    Ok(()) => {}
                    Err(err) => {
                      log::error!("error sending radio config: {err}");
                      break
                    }
                  }
                  // TODO: ack ?
                }
                Err(err) => {
                  log::error!("error deserializing radio config message: {err}");
                  break
                }
              };
            }
            LinkEvent::Activated => {
              log::info!("config link activated {}", link_event.id);
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
