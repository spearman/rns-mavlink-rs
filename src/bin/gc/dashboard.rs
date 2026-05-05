use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::RwLock;

use rns_mavlink::dashboard::{
    error_response, get_page, ok_response, PLUGIN_TLS_CERT_FILE, PLUGIN_TLS_KEY_FILE,
};
use rns_mavlink::gc::{Config, CONFIG_PATH};

const SERVICE_NAME: &str = "rns-mavlink-gc.service";

#[derive(Clone)]
pub struct GcAppState {
    pub config: Arc<RwLock<Config>>,
}

#[derive(Debug, Deserialize)]
struct ConfigUpdate {
    config: String,
}

#[derive(Debug, serde::Serialize)]
struct ConfigResponse {
    config: String,
}

pub fn router(state: GcAppState) -> Router {
    Router::new()
        .route("/", get(handler_get_page))
        .route("/api/config", get(handler_get_config).put(handler_put_config))
        .route("/api/restart", post(handler_restart))
        .with_state(state)
}

async fn handler_get_page() -> Html<String> {
    get_page("RNS-Mavlink GC Dashboard", CONFIG_PATH)
}

async fn handler_get_config(State(state): State<GcAppState>) -> impl IntoResponse {
    let config = state.config.read().await;
    match toml::to_string_pretty(&*config) {
        Ok(config_str) => (StatusCode::OK, Json(ConfigResponse { config: config_str })).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to serialize config: {err}")).into_response(),
    }
}

async fn handler_put_config(
    State(state): State<GcAppState>,
    Json(update): Json<ConfigUpdate>,
) -> impl IntoResponse {
    // Parse the new config
    let new_config: Config = match toml::from_str(&update.config) {
        Ok(cfg) => cfg,
        Err(err) => {
            return error_response(StatusCode::BAD_REQUEST, format!("Invalid TOML: {err}")).into_response();
        }
    };

    // Write to file
    if let Err(err) = std::fs::write(CONFIG_PATH, &update.config) {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to write config file: {err}")).into_response();
    }

    // Update in-memory config
    *state.config.write().await = new_config;

    ok_response("Config saved successfully. Restart the service to apply changes.").into_response()
}

async fn handler_restart() -> impl IntoResponse {
    log::info!("restarting service {SERVICE_NAME}");
    match std::process::Command::new("systemctl")
        .args(["restart", SERVICE_NAME])
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                ok_response(format!("Service {SERVICE_NAME} restart initiated")).into_response()
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("systemctl failed: {stderr}")).into_response()
            }
        }
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to run systemctl: {err}")).into_response(),
    }
}

pub async fn start_server(bind_addr: SocketAddr, state: GcAppState) -> Result<(), String> {
    let app = router(state);

    let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        PLUGIN_TLS_CERT_FILE,
        PLUGIN_TLS_KEY_FILE,
    )
    .await
    .map_err(|err| {
        format!(
            "Failed to load TLS certificates {} and {}: {err}",
            PLUGIN_TLS_CERT_FILE, PLUGIN_TLS_KEY_FILE
        )
    })?;

    log::info!("GC dashboard listening on https://{bind_addr}");

    axum_server::bind_rustls(bind_addr, tls_config)
        .serve(app.into_make_service())
        .await
        .map_err(|err| format!("HTTPS server error: {err}"))
}
