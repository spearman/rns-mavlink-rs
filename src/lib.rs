use std::sync::Arc;

use kaonic_ctrl::error::ControllerError;
use kaonic_reticulum::KaonicCtrlInterface;
use kaonic_reticulum::RadioClient;
use radio_common::RadioConfig;
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
  radio_config: RadioConfig
) -> Result<SharedRadioClient, ControllerError> {
  match KaonicCtrlInterface::connect_client::<1400, 5>(
    "0.0.0.0:0".parse().unwrap(), server_addr, CancellationToken::new()
  ).await {
    Ok(radio_client) => {
      radio_client.lock().await.set_radio_config(radio_module, radio_config).await?;
      // TODO: set modulation
      Ok(radio_client)
    }
    Err(err) => Err(err)
  }
}
