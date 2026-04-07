use std::sync::Arc;

use kaonic_ctrl::error::ControllerError;
use kaonic_reticulum::KaonicCtrlInterface;
use kaonic_reticulum::RadioClient;
use radio_common::{RadioConfig, Modulation};
use radio_common::modulation::OfdmModulation;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub mod fc;
pub mod gc;

pub use fc::Fc;
pub use gc::Gc;

type SharedRadioClient = Arc<Mutex<RadioClient>>;

pub async fn init_kaonic_radio_client(
  server_addr: std::net::SocketAddr,
  radio_module: usize,
  radio_config: RadioConfig,
) -> Result<SharedRadioClient, ControllerError> {
  match KaonicCtrlInterface::connect_client::<1400, 5>(
    "0.0.0.0:0".parse().unwrap(), server_addr, CancellationToken::new()
  ).await {
    Ok(radio_client) => {
      let mut client = radio_client.lock().await;
      client.set_radio_config(radio_module, radio_config).await?;
      let modulation = Modulation::Ofdm(OfdmModulation::default());
      client.set_modulation(radio_module, modulation).await?;
      drop(client);
      Ok(radio_client)
    }
    Err(err) => Err(err)
  }
}
