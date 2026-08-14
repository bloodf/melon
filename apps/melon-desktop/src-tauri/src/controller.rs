use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct ConnectionController;

impl ConnectionController {
    pub fn shutdown(&self) {}
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerStatus {
    pub key_persistence_available: bool,
    pub running: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeInput {
    pub mode: ConnectionMode,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub allow_insecure_http: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionMode {
    ManagedLocal,
    External,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub mode: ConnectionMode,
    pub base_url: String,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, Serialize)]
pub struct ModelInfo {
    pub id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub mode: ConnectionMode,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case", tag = "code", content = "message")]
pub enum ControllerError {
    NotImplemented(String),
}

#[tauri::command]
pub fn status(_controller: tauri::State<'_, ConnectionController>) -> ControllerStatus {
    ControllerStatus { key_persistence_available: false, running: false }
}

#[tauri::command]
pub fn probe(
    _controller: tauri::State<'_, ConnectionController>,
    input: ProbeInput,
) -> Result<ProbeResult, ControllerError> {
    let _ = (input.mode, input.base_url, input.api_key, input.allow_insecure_http);
    Err(ControllerError::NotImplemented("connection probing is not available yet".into()))
}

#[tauri::command]
pub fn activate(
    _controller: tauri::State<'_, ConnectionController>,
    probe: Value,
    model: String,
) -> Result<SavedConnection, ControllerError> {
    let _ = (probe, model);
    Err(ControllerError::NotImplemented("connection activation is not available yet".into()))
}

#[tauri::command]
pub fn shutdown(controller: tauri::State<'_, ConnectionController>) {
    controller.shutdown();
}
