use crate::config::normalize_endpoint;
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_READ_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_OVERALL_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_BODY_LIMIT: usize = 16 * 1_024;
const MODELS_BODY_LIMIT: usize = 1_000_000;

const PROBE_LIMITS: ProbeLimits = ProbeLimits {
    connect_timeout: PROBE_CONNECT_TIMEOUT,
    read_timeout: PROBE_READ_TIMEOUT,
    overall_timeout: PROBE_OVERALL_TIMEOUT,
    health_body_limit: HEALTH_BODY_LIMIT,
    models_body_limit: MODELS_BODY_LIMIT,
};

#[derive(Clone, Copy)]
struct ProbeLimits {
    connect_timeout: Duration,
    read_timeout: Duration,
    overall_timeout: Duration,
    health_body_limit: usize,
    models_body_limit: usize,
}

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

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionMode {
    ManagedLocal,
    External,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub mode: ConnectionMode,
    pub base_url: String,
    pub auth: AuthStatus,
    pub health: HealthStatus,
    pub models: Vec<ModelInfo>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthStatus {
    Verified,
    NotRequired,
    Unavailable,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthStatus {
    Healthy,
    NotExposed,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    pub mode: ConnectionMode,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ControllerError {
    ApiKeyRequired,
    AuthProbeFailed,
    EmptyModels,
    HealthProbeFailed,
    InsecureHttpConfirmationRequired,
    InvalidEndpoint,
    MalformedModels,
    ModelsRequestFailed,
    ModelsResponseTooLarge,
    NotImplemented(String),
    ProbeFailed,
    ProbeTimedOut,
}

impl Serialize for ControllerError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let (code, message) = match self {
            Self::ApiKeyRequired => ("auth-required", "API key required"),
            Self::AuthProbeFailed => ("auth-unverified", "API key could not be verified"),
            Self::EmptyModels => ("empty-models", "DurinDoor returned no models"),
            Self::HealthProbeFailed => ("health-failed", "DurinDoor health check failed"),
            Self::InsecureHttpConfirmationRequired => {
                ("insecure-http", "Insecure HTTP requires explicit confirmation")
            }
            Self::InvalidEndpoint => ("invalid-endpoint", "Invalid DurinDoor endpoint"),
            Self::MalformedModels | Self::ModelsResponseTooLarge => {
                ("malformed-models", "DurinDoor returned an invalid model list")
            }
            Self::ModelsRequestFailed => ("models-failed", "DurinDoor model request failed"),
            Self::NotImplemented(message) => ("not-implemented", message.as_str()),
            Self::ProbeFailed => ("probe-failed", "DurinDoor probe failed"),
            Self::ProbeTimedOut => ("probe-timed-out", "DurinDoor probe timed out"),
        };
        let mut state = serializer.serialize_struct("ControllerError", 2)?;
        state.serialize_field("code", code)?;
        state.serialize_field("message", message)?;
        state.end()
    }
}

#[tauri::command]
pub fn status(_controller: tauri::State<'_, ConnectionController>) -> ControllerStatus {
    ControllerStatus { key_persistence_available: false, running: false }
}

#[tauri::command]
pub async fn probe(
    _controller: tauri::State<'_, ConnectionController>,
    input: ProbeInput,
) -> Result<ProbeResult, ControllerError> {
    match input.mode {
        ConnectionMode::External => probe_external(&input, PROBE_LIMITS).await,
        ConnectionMode::ManagedLocal => {
            Err(ControllerError::NotImplemented("managed probing is not available yet".into()))
        }
    }
}

/// Probes one normalized external DurinDoor endpoint without persisting configuration or secrets.
async fn probe_external(input: &ProbeInput, limits: ProbeLimits) -> Result<ProbeResult, ControllerError> {
    let base_url = input.base_url.as_deref().ok_or(ControllerError::InvalidEndpoint)?;
    let normalized = normalize_endpoint(base_url).map_err(|_| ControllerError::InvalidEndpoint)?;
    if normalized.requires_insecure_confirmation && input.allow_insecure_http != Some(true) {
        return Err(ControllerError::InsecureHttpConfirmationRequired);
    }
    let base = Url::parse(normalized.as_str()).map_err(|_| ControllerError::InvalidEndpoint)?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(limits.connect_timeout)
        .read_timeout(limits.read_timeout)
        .build()
        .map_err(|_| ControllerError::ProbeFailed)?;
    tokio::time::timeout(limits.overall_timeout, probe_with_client(&client, input, base, limits))
        .await
        .map_err(|_| ControllerError::ProbeTimedOut)?
}

async fn probe_with_client(
    client: &Client,
    input: &ProbeInput,
    base: Url,
    limits: ProbeLimits,
) -> Result<ProbeResult, ControllerError> {
    let health_response = send(client, endpoint_url(&base, "api/health")?, input.api_key.as_deref()).await?;
    let health = match health_response.status() {
        StatusCode::OK => {
            let body = read_capped(health_response, limits.health_body_limit, ControllerError::HealthProbeFailed).await?;
            let value: Value = serde_json::from_slice(&body).map_err(|_| ControllerError::HealthProbeFailed)?;
            if value.get("ok") == Some(&Value::Bool(true)) {
                HealthStatus::Healthy
            } else {
                return Err(ControllerError::HealthProbeFailed);
            }
        }
        StatusCode::NOT_FOUND => HealthStatus::NotExposed,
        _ => return Err(ControllerError::HealthProbeFailed),
    };

    let auth_response = send(
        client,
        endpoint_url(&base, "api/v1/realtime/auth")?,
        input.api_key.as_deref(),
    )
    .await?;
    let auth = match auth_response.status() {
        StatusCode::OK if input.api_key.as_deref().is_some_and(|key| !key.is_empty()) => AuthStatus::Verified,
        StatusCode::OK => AuthStatus::NotRequired,
        StatusCode::UNAUTHORIZED => return Err(ControllerError::ApiKeyRequired),
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => AuthStatus::Unavailable,
        _ => return Err(ControllerError::AuthProbeFailed),
    };

    let models_response = send(client, endpoint_url(&base, "models")?, input.api_key.as_deref()).await?;
    if models_response.status() == StatusCode::UNAUTHORIZED {
        return Err(ControllerError::ApiKeyRequired);
    }
    if models_response.status() != StatusCode::OK {
        return Err(ControllerError::ModelsRequestFailed);
    }
    let body = read_capped(
        models_response,
        limits.models_body_limit,
        ControllerError::ModelsResponseTooLarge,
    )
    .await?;
    #[derive(Deserialize)]
    struct Catalog {
        data: Vec<ModelInfo>,
    }
    let catalog: Catalog = serde_json::from_slice(&body).map_err(|_| ControllerError::MalformedModels)?;
    if catalog.data.is_empty() {
        return Err(ControllerError::EmptyModels);
    }
    if catalog.data.iter().any(|model| model.id.is_empty()) {
        return Err(ControllerError::MalformedModels);
    }
    Ok(ProbeResult {
        mode: ConnectionMode::External,
        base_url: base.to_string(),
        auth,
        health,
        models: catalog.data,
    })
}

fn endpoint_url(base: &Url, route: &str) -> Result<Url, ControllerError> {
    let mut endpoint = base.clone();
    let base_path = base.path().trim_end_matches("/v1").trim_end_matches('/');
    let path = if route == "models" {
        format!("{}/v1/models", base_path)
    } else {
        format!("{base_path}/{route}")
    };
    endpoint.set_path(&path);
    Ok(endpoint)
}

async fn send(client: &Client, url: Url, api_key: Option<&str>) -> Result<Response, ControllerError> {
    let request = client.get(url);
    let request = match api_key.filter(|key| !key.is_empty()) {
        Some(api_key) => request.bearer_auth(api_key),
        None => request,
    };
    request.send().await.map_err(map_request_error)
}

async fn read_capped(
    mut response: Response,
    limit: usize,
    oversized: ControllerError,
) -> Result<Vec<u8>, ControllerError> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(oversized);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(map_request_error)? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(oversized);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn map_request_error(error: reqwest::Error) -> ControllerError {
    if error.is_timeout() {
        ControllerError::ProbeTimedOut
    } else {
        ControllerError::ProbeFailed
    }
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

#[cfg(test)]
mod tests {
    use super::{
        AuthStatus, ControllerError, HealthStatus, ProbeInput, ProbeLimits, probe_external,
    };
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream, UdpSocket};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    const FAST_LIMITS: ProbeLimits = ProbeLimits {
        connect_timeout: Duration::from_millis(100),
        read_timeout: Duration::from_millis(50),
        overall_timeout: Duration::from_millis(150),
        health_body_limit: 1_024,
        models_body_limit: 1_024,
    };

    #[derive(Clone)]
    struct Reply {
        status: u16,
        body: Vec<u8>,
        delay: Duration,
    }

    impl Reply {
        fn json(body: &str) -> Self {
            Self { status: 200, body: body.as_bytes().to_vec(), delay: Duration::ZERO }
        }

        fn status(status: u16) -> Self {
            Self { status, body: Vec::new(), delay: Duration::ZERO }
        }
    }

    struct Server {
        port: u16,
        requests: Arc<Mutex<Vec<(String, Option<String>)>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn start(health: Reply, auth: Reply, models: Reply) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind fixture");
            listener.set_nonblocking(true).expect("nonblocking fixture");
            let port = listener.local_addr().expect("fixture address").port();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let observed = Arc::clone(&requests);
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = Arc::clone(&stop);
            let thread = thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &health, &auth, &models, &observed),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self { port, requests, stop, thread: Some(thread) }
        }

        fn url(&self, host: &str, prefix: &str) -> String {
            format!("http://{host}:{}{prefix}", self.port)
        }

        fn observed(&self) -> Vec<(String, Option<String>)> {
            self.requests.lock().expect("request log").clone()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
            if let Some(thread) = self.thread.take() {
                thread.join().expect("fixture thread");
            }
        }
    }

    fn serve(
        mut stream: TcpStream,
        health: &Reply,
        auth: &Reply,
        models: &Reply,
        requests: &Mutex<Vec<(String, Option<String>)>>,
    ) {
        stream.set_read_timeout(Some(Duration::from_secs(1))).expect("fixture timeout");
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 1_024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut chunk).expect("read request");
            if count == 0 {
                return;
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let request = String::from_utf8_lossy(&bytes);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request path")
            .to_owned();
        let authorization = request.lines().find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("authorization").then(|| value.trim().to_owned())
            })
        });
        requests.lock().expect("request log").push((path.clone(), authorization));
        let reply = if path.ends_with("/api/health") {
            health
        } else if path.ends_with("/api/v1/realtime/auth") {
            auth
        } else if path.ends_with("/v1/models") {
            models
        } else {
            &Reply::status(404)
        };
        thread::sleep(reply.delay);
        let reason = match reply.status {
            200 => "OK",
            401 => "Unauthorized",
            404 => "Not Found",
            405 => "Method Not Allowed",
            _ => "Error",
        };
        if write!(
            stream,
            "HTTP/1.1 {} {reason}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n",
            reply.status
        )
        .is_err()
        {
            return;
        }
        for body_chunk in reply.body.chunks(128) {
            if stream.write_all(body_chunk).is_err() {
                break;
            }
        }
    }

    fn local_ip() -> std::net::IpAddr {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind UDP");
        socket.connect("192.0.2.1:9").expect("select local route");
        socket.local_addr().expect("local route").ip()
    }

    fn input(base_url: String, api_key: Option<&str>, allow_insecure_http: bool) -> ProbeInput {
        ProbeInput {
            mode: super::ConnectionMode::External,
            base_url: Some(base_url),
            api_key: api_key.map(str::to_owned),
            allow_insecure_http: Some(allow_insecure_http),
        }
    }

    fn run(input: &ProbeInput) -> Result<super::ProbeResult, ControllerError> {
        tauri::async_runtime::block_on(probe_external(input, FAST_LIMITS))
    }

    #[test]
    fn blocks_insecure_non_loopback_http_before_request() {
        let server = Server::start(
            Reply::json(r#"{"ok":true}"#),
            Reply::status(200),
            Reply::json(r#"{"data":[{"id":"model-a"}]}"#),
        );
        let result = run(&input(server.url(&local_ip().to_string(), ""), None, false));
        assert_eq!(result, Err(ControllerError::InsecureHttpConfirmationRequired));
        assert!(server.observed().is_empty());
    }

    #[test]
    fn preserves_deployment_prefix_and_warns_when_health_is_missing() {
        let server = Server::start(
            Reply::status(404),
            Reply::status(200),
            Reply::json(r#"{"data":[{"id":"model-a","name":"Model A"}]}"#),
        );
        let result = run(&input(server.url("127.0.0.1", "/gateway"), None, false)).unwrap();
        assert_eq!(result.health, HealthStatus::NotExposed);
        assert_eq!(result.auth, AuthStatus::NotRequired);
        assert_eq!(result.models[0].name.as_deref(), Some("Model A"));
        assert_eq!(
            server.observed().into_iter().map(|(path, _)| path).collect::<Vec<_>>(),
            [
                "/gateway/api/health",
                "/gateway/api/v1/realtime/auth",
                "/gateway/v1/models",
            ]
        );
    }

    #[test]
    fn maps_authentication_statuses_without_exposing_key_in_errors() {
        for key in [None, Some("top-secret")] {
            let required = Server::start(
                Reply::json(r#"{"ok":true}"#),
                Reply::status(401),
                Reply::json(r#"{"data":[{"id":"model-a"}]}"#),
            );
            let error = run(&input(required.url("127.0.0.1", ""), key, false)).unwrap_err();
            assert_eq!(error, ControllerError::ApiKeyRequired);
            assert!(!format!("{error:?}").contains("top-secret"));
        }

        for status in [404, 405] {
            let unsupported = Server::start(
                Reply::json(r#"{"ok":true}"#),
                Reply::status(status),
                Reply::json(r#"{"data":[{"id":"model-a"}]}"#),
            );
            let result = run(&input(unsupported.url("127.0.0.1", ""), Some("top-secret"), false)).unwrap();
            assert_eq!(result.auth, AuthStatus::Unavailable);
        }
    }

    #[test]
    fn serializes_recoverable_errors_for_existing_frontend_payload() {
        assert_eq!(
            serde_json::to_value(ControllerError::ApiKeyRequired).unwrap(),
            serde_json::json!({ "code": "auth-required", "message": "API key required" })
        );
        assert_eq!(
            serde_json::to_value(ControllerError::EmptyModels).unwrap(),
            serde_json::json!({ "code": "empty-models", "message": "DurinDoor returned no models" })
        );
        assert_eq!(
            serde_json::to_value(ControllerError::ModelsResponseTooLarge).unwrap(),
            serde_json::json!({ "code": "malformed-models", "message": "DurinDoor returned an invalid model list" })
        );
    }

    #[test]
    fn sends_bearer_only_when_key_is_present() {
        for (key, expected) in [
            (None, None),
            (Some(""), None),
            (Some("top-secret"), Some("Bearer top-secret")),
        ] {
            let server = Server::start(
                Reply::json(r#"{"ok":true}"#),
                Reply::status(200),
                Reply::json(r#"{"data":[{"id":"model-a"}]}"#),
            );
            run(&input(server.url("127.0.0.1", ""), key, false)).unwrap();
            let headers = server.observed().into_iter().map(|(_, header)| header).collect::<Vec<_>>();
            assert_eq!(headers, vec![expected.map(str::to_owned); 3]);
        }
    }

    #[test]
    fn accepts_valid_non_empty_models() {
        let server = Server::start(
            Reply::json(r#"{"ok":true}"#),
            Reply::status(200),
            Reply::json(r#"{"data":[{"id":"model-a"},{"id":"model-b","name":"Model B"}]}"#),
        );
        let result = run(&input(server.url("127.0.0.1", "/v1"), None, false)).unwrap();
        assert_eq!(result.base_url, server.url("127.0.0.1", "/v1"));
        assert_eq!(result.models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), ["model-a", "model-b"]);
    }

    #[test]
    fn rejects_empty_and_malformed_models() {
        for (body, expected) in [
            (r#"{"data":[]}"#, ControllerError::EmptyModels),
            (r#"{"data":[{"name":"missing id"}]}"#, ControllerError::MalformedModels),
            ("{not-json", ControllerError::MalformedModels),
        ] {
            let server = Server::start(
                Reply::json(r#"{"ok":true}"#),
                Reply::status(200),
                Reply::json(body),
            );
            assert_eq!(run(&input(server.url("127.0.0.1", ""), None, false)), Err(expected));
        }
    }

    #[test]
    fn caps_models_while_reading() {
        let oversized = format!(r#"{{"data":[{{"id":"{}"}}]}}"#, "x".repeat(FAST_LIMITS.models_body_limit));
        let server = Server::start(
            Reply::json(r#"{"ok":true}"#),
            Reply::status(200),
            Reply::json(&oversized),
        );
        assert_eq!(
            run(&input(server.url("127.0.0.1", ""), None, false)),
            Err(ControllerError::ModelsResponseTooLarge)
        );
    }

    #[test]
    fn rejects_delayed_models_with_short_test_timeout() {
        let server = Server::start(
            Reply::json(r#"{"ok":true}"#),
            Reply::status(200),
            Reply {
                status: 200,
                body: br#"{"data":[{"id":"model-a"}]}"#.to_vec(),
                delay: Duration::from_millis(100),
            },
        );
        let limits = ProbeLimits {
            read_timeout: Duration::from_secs(1),
            overall_timeout: Duration::from_millis(50),
            ..FAST_LIMITS
        };
        let input = input(server.url("127.0.0.1", ""), None, false);
        assert_eq!(
            tauri::async_runtime::block_on(probe_external(&input, limits)),
            Err(ControllerError::ProbeTimedOut)
        );
    }
}
