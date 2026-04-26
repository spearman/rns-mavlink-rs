use std::sync::Arc;

use kaonic_ctrl::error::ControllerError;
use kaonic_reticulum::KaonicCtrlInterface;
use kaonic_reticulum::RadioClient;
use radio_common::{RadioConfig, Modulation};
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
    log::info!("packets in / s: {in_pps}, packets out / s: {out_pps}, \
      link in B/s: {in_bps}, link out B/s: {out_bps}, \
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
