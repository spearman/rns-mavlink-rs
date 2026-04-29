use std::sync::Arc;

use chrono;
use kaonic_ctrl::error::ControllerError;
use kaonic_reticulum::KaonicCtrlInterface;
use kaonic_reticulum::RadioClient;
use radio_common::{RadioConfig, Modulation};
use rolling_file::BasicRollingFileAppender;
use serde_json;
use tokio::sync::Mutex;
use tokio::time;
use tokio_util::sync::CancellationToken;

pub mod fc;
pub mod gc;

pub use fc::Fc;
pub use gc::Gc;

pub(crate) const THROUGHPUT_LOG_FREQUENCY_SECONDS: u64 = 2;

type SharedRadioClient = Arc<Mutex<RadioClient>>;

pub async fn init_kaonic_radio_client(
  listen_addr: std::net::SocketAddr,
  server_addr: std::net::SocketAddr,
  radio_module: usize,
  radio_config: RadioConfig,
  radio_modulation: Modulation
) -> Result<SharedRadioClient, ControllerError> {
  match KaonicCtrlInterface::connect_client::<1400, 5>(
    listen_addr, server_addr, CancellationToken::new()
  ).await {
    Ok(radio_client) => {
      let mut client = radio_client.lock().await;
      client.set_radio_config(radio_module, radio_config).await?;
      client.set_modulation(radio_module, radio_modulation).await?;
      drop(client);
      Ok(radio_client)
    }
    Err(err) => Err(err)
  }
}

pub struct MavlinkParser {
  pub reader: mavlink::peek_reader::PeekReader<bytes::buf::Reader<bytes::BytesMut>>,
  pub buffer: Vec<mavlink::MavFrame<mavlink::common::MavMessage>>
}

impl MavlinkParser {
  pub fn new() -> Self {
    use bytes::Buf;
    MavlinkParser {
      reader: mavlink::peek_reader::PeekReader::new(bytes::BytesMut::new().reader()),
      buffer: Vec::new()
    }
  }

  pub fn parse(&mut self, message: &[u8])
    -> std::vec::Drain<'_, mavlink::MavFrame<mavlink::common::MavMessage>>
  {
    use bytes::BufMut;
    use mavlink::Message;
    self.reader.reader_mut().get_mut().put(message);
    loop {
      match mavlink::read_any_raw_message::<mavlink::common::MavMessage, _>(
        &mut self.reader
      ) {
        Ok(raw) => {
          let protocol_version = raw.version();
          let header = mavlink::MavHeader {
            system_id: raw.system_id(),
            component_id: raw.component_id(),
            sequence: raw.sequence()
          };
          match mavlink::common::MavMessage::parse(
            protocol_version, raw.message_id(), raw.payload()
          ) {
            Ok(msg) =>
              self.buffer.push (mavlink::MavFrame { protocol_version, header, msg }),
            Err(err) => {
              log::warn!("error parsing mavlink message: {err:?}");
              break
            }
          };
        }
        Err(err) => {
          match err {
            mavlink::error::MessageReadError::Io(err)
              if err.kind() == std::io::ErrorKind::UnexpectedEof => {}
            err => log::warn!("error reading mavlink message: {err:?}")
          }
          break
        }
      }
    }
    self.buffer.drain(..)
  }
}

pub async fn log_mavlink<'a>(
  logfile: Arc<Mutex<BasicRollingFileAppender>>,
  source: &'static str,
  frames: std::vec::Drain<'a, mavlink::MavFrame<mavlink::common::MavMessage>>
) {
  use std::io::Write;
  let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
  let mut log = logfile.lock().await;
  for frame in frames {
    match serde_json::to_string(&frame)
      .map_err(|err| err.to_string())
      .and_then(|frame|
        log.write_fmt(format_args!(
          "{{\"ts\":{ts:?},\"source\":{source:?},\"frame\":{frame}}}\n"))
          .map_err(|err| err.to_string()))
    {
      Ok(_) => {}
      Err(err) => {
        log::error!("error writing to mavlink log: {err}");
        return
      }
    }
  }
}

pub(crate) struct Throughput {
  is_gc: bool,
  is_fc: bool,
  /// Total data link outgoing bytes since startup
  link_send_total_bytes: u64,
  /// Data link outgoing bytes since last log
  link_send_bytes: u32,
  /// Total data link incoming bytes since startup
  link_recv_total_bytes: u64,
  /// Data link incoming bytes since last log
  link_recv_bytes: u32,
  packets_in: u32,
  packets_out: u32,
  packets_in_total: u64,
  packets_out_total: u64,
  total_ground_station_bytes: u64,
  total_serial_port_bytes: u64,
  last_ts: time::Instant
}

impl Throughput {
  pub fn new_gc() -> Self {
    Throughput {
      is_gc: true,
      is_fc: false,
      link_send_total_bytes: 0,
      link_send_bytes: 0,
      link_recv_total_bytes: 0,
      link_recv_bytes: 0,
      packets_in: 0,
      packets_out: 0,
      packets_in_total: 0,
      packets_out_total: 0,
      total_ground_station_bytes: 0,
      total_serial_port_bytes: 0,
      last_ts: time::Instant::now()
    }
  }

  pub fn new_fc() -> Self {
    Throughput {
      is_gc: false,
      is_fc: true,
      link_send_total_bytes: 0,
      link_send_bytes: 0,
      link_recv_total_bytes: 0,
      link_recv_bytes: 0,
      packets_in: 0,
      packets_out: 0,
      packets_in_total: 0,
      packets_out_total: 0,
      total_ground_station_bytes: 0,
      total_serial_port_bytes: 0,
      last_ts: time::Instant::now()
    }
  }

  pub fn recv_packet(&mut self, n : u32) {
    self.link_recv_bytes += n;
    self.link_recv_total_bytes += n as u64;
    self.packets_in += 1;
    self.packets_in_total += 1;
  }
  pub fn send_packet(&mut self, n : u32) {
    self.link_send_bytes += n;
    self.link_send_total_bytes += n as u64;
    self.packets_out += 1;
    self.packets_out_total += 1;
  }
  pub fn ground_station_bytes(&mut self, n : u64) {
    self.total_ground_station_bytes += n;
  }
  pub fn serial_port_bytes(&mut self, n : u64) {
    self.total_serial_port_bytes += n;
  }
  pub fn log(&mut self) {
    let now = time::Instant::now();
    let elapsed = now - self.last_ts;
    let in_bps = (self.link_recv_bytes as f32 / elapsed.as_secs_f32()) as u32;
    let out_bps = (self.link_send_bytes as f32 / elapsed.as_secs_f32()) as u32;
    let in_pps = (self.packets_in as f32 / elapsed.as_secs_f32()) as u32;
    let out_pps = (self.packets_out as f32 / elapsed.as_secs_f32()) as u32;
    let extra_string = if self.is_gc {
      format!(", total ground station bytes: {}", self.total_ground_station_bytes)
    } else if self.is_fc {
      format!(", total serial port bytes: {}", self.total_serial_port_bytes)
    } else {
      "".to_string()
    };
    log::info!("link in B/s: {in_bps}, link out B/s: {out_bps}, \
      packets in / s: {in_pps}, packets out / s: {out_pps}, \
      total packets in: {}, total packets out: {} \
      total bytes in: {}, total bytes out: {}\
      {extra_string}",
      self.packets_in_total, self.packets_out_total, self.link_recv_total_bytes,
      self.link_send_total_bytes);
    self.last_ts = now;
    self.link_recv_bytes = 0;
    self.link_send_bytes = 0;
    self.packets_in = 0;
    self.packets_out = 0;
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn parse_mavlink() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("INFO"))
      .is_test(true)
      .init();
    log::info!("TEST");
    let bytes1 = [3, 81, 3, 3, 80, 133, 253, 9, 0, 0, 173, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 64, 11, 253, 9, 0, 0, 174, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 97, 145, 253, 9, 0, 0, 175, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 113, 31, 253, 9, 0, 0, 176, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 126, 92, 253, 9, 0, 0, 177, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 110, 210, 253, 9, 0, 0, 178, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81];
    let bytes2 = [3, 3, 79, 72, 253, 13, 0, 0, 179, 1, 1, 111, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 89, 175, 33, 67, 37, 199, 217, 253, 9, 0, 0, 180, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 28, 116, 253, 9, 0, 0, 181, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 12, 250, 253, 9, 0, 0, 182, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 45, 96, 253, 9, 0, 0, 183, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 61, 238, 253, 9, 0, 0, 184, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 186, 12, 253, 9, 0, 0, 185, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 170, 130, 253, 9, 0, 0, 186, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 139, 24, 253, 9, 0, 0, 187, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 155, 150, 253, 9, 0, 0, 188, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 216, 36, 253, 9, 0, 0, 189, 1, 1, 0, 0, 0, 0, 0, 0, 0, 2, 3, 81, 3, 3, 200, 170, 253, 13, 0, 0, 190, 1, 1, 111, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut parser = MavlinkParser::new();
    let mut seqnums = vec![];
    for frame in parser.parse(&bytes1[..]) {
      println!("FRAME1: {frame:?}");
      seqnums.push(frame.header.sequence);
    }
    for frame in parser.parse(&bytes2[..]) {
      println!("FRAME2: {frame:?}");
      seqnums.push(frame.header.sequence);
    }
    assert_eq!(seqnums, (173..190).collect::<Vec<_>>());
  }
}
