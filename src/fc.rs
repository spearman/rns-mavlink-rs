use std::sync::Arc;

use radio_common::{Modulation, RadioConfig};
use radio_common::modulation::OfdmModulation;
use serde::{Deserialize, Serialize};
use tokio;
use tokio::time;
use tokio::sync::Mutex;

use reticulum::destination::link::{Link, LinkEvent, LinkStatus};
use reticulum::transport::Transport;
use reticulum::hash::AddressHash;

use crate::{SharedRadioClient, Throughput, THROUGHPUT_LOG_FREQUENCY_SECONDS};

pub const CONFIG_PATH: &str = "Fc.toml";
/// When the fc is disconnected the port will read 0 bytes and reconnecting the
/// controller does not always restore the data stream because it may be assigned to a
/// different serial device in `/dev/`. If we detect this is happening then shut down
/// the flight controller.
const SERIAL_PORT_READ_0_BYTES_LIMIT: usize = 30;
const DATA_LINK_REQUEST_TIMEOUT_SECONDS: u64 = 15;

fn default_radio_modulation() -> Modulation {
  Modulation::Ofdm(OfdmModulation::default())
}

pub struct Fc {
  config: Config,
  radio_client: Option<SharedRadioClient>
}

#[derive(Deserialize, Serialize)]
pub struct Config {
  pub log_level: String,
  #[serde(default)]
  pub log_throughput: bool,
  pub serial_port: String,
  pub serial_baud: u32,
  // TODO: deserialize AddressHash
  pub gc_data_destination: String,
  pub radio_module: usize,
  pub radio_config: RadioConfig,
  #[serde(default="default_radio_modulation")]
  pub radio_modulation: Modulation
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
    type MaybeLink = Arc<Mutex<Option<Arc<Mutex<Link>>>>>;
    let data_link: MaybeLink = Arc::new(Mutex::new(None));
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
    let throughput = Arc::new(Mutex::new(Throughput::new()));
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
    let data_link_request_ts: Arc<Mutex<Option<time::Instant>>> =
      Arc::new(Mutex::new(None));
    let link_loop = async || {
      const ANNOUNCE_RECV_TIMEOUT: time::Duration = time::Duration::from_secs(5);
      let mut announce_recv = transport.recv_announces().await;
      let check_status = async || {
        if let Some(link) = data_link.lock().await.as_ref() {
          if link.lock().await.status() == LinkStatus::Closed {
            log::warn!("data link is closed, discarding link");
            if let Some(link) = data_link.lock().await.take() {
              let link_id = link.lock().await.id().clone();
              let _ = transport.link_close(link_id).await.map_err(|err|
                log::warn!("error closing data link {link_id}: {err:?}"));
            }
            *data_link_request_ts.lock().await = None;
          }
        }
        if let Some(link_request_ts) = data_link_request_ts.lock().await.as_ref() {
          if link_request_ts.elapsed() >
            time::Duration::from_secs (DATA_LINK_REQUEST_TIMEOUT_SECONDS)
          {
            log::warn!("requested data link still not active after \
              {DATA_LINK_REQUEST_TIMEOUT_SECONDS} seconds, discarding link");
            if let Some(link) = data_link.lock().await.take() {
              let link_id = link.lock().await.id().clone();
              let _ = transport.link_close(link_id).await.map_err(|err|
                log::warn!("error closing data link {link_id}: {err:?}"));
            }
            *data_link_request_ts.lock().await = None;
          }
        }
      };
      loop {
        match time::timeout(ANNOUNCE_RECV_TIMEOUT, announce_recv.recv()).await {
          Ok(Ok(announce)) => {
            check_status().await;
            let destination = announce.destination.lock().await;
            let address = destination.desc.address_hash;
            if address == gc_data_destination {
              log::debug!("got gc data destination announce {address}");
              let mut data_link = data_link.lock().await;
              if data_link.is_none() {
                log::info!("creating data link for {address}");
                *data_link = Some(transport.link(destination.desc).await);
                *data_link_request_ts.lock().await = Some(time::Instant::now());
              }
            } else {
              log::debug!("got announce: {address}");
            }
          }
          Ok(Err(err)) => {
            log::error!("error receiving announces: {err}");
            break
          }
          Err(_) => check_status().await
        }
      }
    };
    // read serial port and forward to links
    let mut read_port_loop = async || {
      log::info!("reading from serial port {}", serial_port);
      let mut buf = vec![0u8; 2usize.pow(16)];
      let mut read_0_bytes_counter = 0;
      loop {
        match port_reader.read(&mut buf).await {
          Ok(n) => {
            log::trace!("read {n} bytes");
            if n > 0 {
              read_0_bytes_counter = 0;
            } else {
              read_0_bytes_counter += 1;
              if read_0_bytes_counter == SERIAL_PORT_READ_0_BYTES_LIMIT {
                log::error!("serial port read 0 bytes limit reached \
                  ({SERIAL_PORT_READ_0_BYTES_LIMIT}), shutting down");
                return
              }
            }
            if let Some(link_mutex) = data_link.lock().await.as_ref() {
              for data in buf[..n].chunks(reticulum::packet::PACKET_MDU / 2) {
                let link = link_mutex.lock().await;
                match link.status() {
                  LinkStatus::Closed => {
                    log::warn!("link closed, not sending");
                    break
                  }
                  LinkStatus::Pending | LinkStatus::Handshake => {
                    log::debug!("link pending, not sending");
                    break
                  }
                  LinkStatus::Active | LinkStatus::Stale => {}
                }
                log::trace!("sending on link ({})", link.id());
                match link.data_packet(data) {
                  Ok(packet) => {
                    drop(link); // drop before sending to prevent deadlock
                    transport.send_packet(packet).await;
                    if self.config.log_throughput {
                      throughput.lock().await.send_bytes(data.len() as u32);
                    }
                  }
                  Err(err) => log::warn!("error creating data packet: {err:?}")
                }
              }
            }
          }
          Err(e) => log::error!("error reading serial port: {}", e)
        }
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
                  if self.config.log_throughput {
                    throughput.lock().await.recv_bytes(payload.len() as u32);
                  }
                }
                LinkEvent::Activated => {
                  log::info!("data link activated {}", link_event.id);
                  debug_assert!(data_link.lock().await.is_some());
                  debug_assert!(data_link_request_ts.lock().await.is_some());
                  *data_link_request_ts.lock().await = None;
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
    // throughput log
    let throughput_log_loop = async || {
      if self.config.log_throughput {
        loop {
          time::sleep(time::Duration::from_secs(THROUGHPUT_LOG_FREQUENCY_SECONDS)).await;
          throughput.lock().await.log();
        }
      } else {
        /*FIXME:debug*/ log::warn!("======================= BANG1");
        std::future::pending::<()>().await;
        /*FIXME:debug*/ log::warn!("----------------------- BANG2");
      }
    };
    // run
    tokio::select!{
      _ = throughput_log_loop() =>
        log::info!("throughput log loop exited: shutting down"),
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
