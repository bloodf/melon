use crate::config::normalize_endpoint;
use crate::process_tree::ProcessTree;
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use std::sync::{Mutex, MutexGuard};
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

const PROCESS_STOP_GRACE: Duration = Duration::from_secs(2);
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(10_250 + 4 * 6_000);
const MAX_OWNED_TREES: u32 = 4;
const RECOVERABLE_CLEANUP: &str = "A started process could not be stopped; retry shutdown";
const RECOVERABLE_QUERY: &str = "A started process could not be checked; retry shutdown";
const RECOVERABLE_SHUTDOWN: &str = "Started process shutdown did not complete; retry shutdown";


trait OwnedTree: Send {
    fn is_running(&mut self) -> io::Result<bool>;
    fn stop(&mut self, grace: Duration) -> io::Result<()>;
}

impl OwnedTree for ProcessTree {
    fn is_running(&mut self) -> io::Result<bool> { ProcessTree::is_running(self) }
    fn stop(&mut self, grace: Duration) -> io::Result<()> { ProcessTree::stop(self, grace) }
}

#[derive(Default)]
struct OwnedProcesses {
    harness: Option<Box<dyn OwnedTree>>,
    managed_durindoor: Option<Box<dyn OwnedTree>>,
}
impl OwnedProcesses {
    fn is_empty(&self) -> bool {
        self.harness.is_none() && self.managed_durindoor.is_none()
    }
}


impl std::fmt::Debug for OwnedProcesses {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedProcesses")
            .field("harness", &self.harness.is_some())
            .field("managed_durindoor", &self.managed_durindoor.is_some())
            .finish()
    }
}

#[derive(Debug, Default)]
struct ControllerState {
    mutating: bool,
    shutting_down: bool,
    retirement_active: bool,
    shutdown_active: bool,
    teardown_harness: bool,
    teardown_managed: bool,
    teardown_retiring: bool,
    owned: OwnedProcesses,
    retiring: Vec<OwnedProcesses>,
    recoverable_error: Option<String>,
}

#[derive(Debug)]
pub struct ConnectionController {
    state: Mutex<ControllerState>,
    key_persistence_available: bool,
    stop_grace: Duration,
    shutdown_budget: Duration,
}

impl Default for ConnectionController {
    fn default() -> Self {
        Self {
            state: Mutex::new(ControllerState::default()),
            key_persistence_available: false,
            stop_grace: PROCESS_STOP_GRACE,
            shutdown_budget: SHUTDOWN_BUDGET,
        }
    }
}

impl ConnectionController {
    fn lock(&self) -> MutexGuard<'_, ControllerState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn begin_operation(&self) -> Result<Operation<'_>, ControllerError> {
        let mut state = self.lock();
        if state.shutting_down {
            return Err(ControllerError::ShuttingDown);
        }
        if state.retirement_active || !state.retiring.is_empty() {
            return Err(ControllerError::CleanupPending);
        }
        if state.mutating {
            return Err(ControllerError::Busy);
        }
        state.mutating = true;
        Ok(Operation { controller: self, active: true })
    }

    #[cfg(test)]
    fn run_serialized<T>(&self, work: impl FnOnce() -> T) -> Result<T, ControllerError> {
        let operation = self.begin_operation()?;
        let result = work();
        drop(operation);
        Ok(result)
    }

    fn status_snapshot(&self) -> ControllerStatus {
        let mut state = self.lock();
        let mut query_error = false;
        let mut running = state.teardown_harness
            || state.teardown_managed
            || state.teardown_retiring
            || state.retirement_active;
        query_owned_processes(&mut state.owned, &mut running, &mut query_error);
        for owned in &mut state.retiring {
            query_owned_processes(owned, &mut running, &mut query_error);
        }
        state.retiring.retain(|owned| !owned.is_empty());
        if query_error && state.recoverable_error.is_none() {
            state.recoverable_error = Some(RECOVERABLE_QUERY.into());
        }
        ControllerStatus {
            key_persistence_available: self.key_persistence_available,
            running,
            recoverable_error: state.recoverable_error.clone(),
        }
    }
    pub fn shutdown(&self) -> Result<(), ControllerError> {
        let deadline = std::time::Instant::now() + self.shutdown_budget;
        let mut owns_shutdown = false;
        let mut owned_sets = loop {
            let mut state = self.lock();
            state.shutting_down = true;
            if !owns_shutdown {
                if state.shutdown_active {
                    return Err(ControllerError::Busy);
                }
                state.shutdown_active = true;
                owns_shutdown = true;
            }
            if state.retirement_active || state.mutating {
                drop(state);
                if std::time::Instant::now() >= deadline {
                    let mut state = self.lock();
                    state.shutdown_active = false;
                    state.recoverable_error = Some(RECOVERABLE_SHUTDOWN.into());
                    return Err(ControllerError::ShutdownFailed);
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            let current = std::mem::take(&mut state.owned);
            let mut owned_sets = std::mem::take(&mut state.retiring);
            if !current.is_empty() {
                owned_sets.push(current);
            }
            state.teardown_harness = owned_sets.iter().any(|owned| owned.harness.is_some());
            state.teardown_managed = owned_sets.iter().any(|owned| owned.managed_durindoor.is_some());
            state.teardown_retiring = !owned_sets.is_empty();
            break owned_sets;
        };

        let mut first_error = None;
        for owned in &mut owned_sets {
            if let Some(error) = stop_and_prune_until(owned, self.stop_grace, deadline) {
                first_error.get_or_insert(error);
            }
        }
        owned_sets.retain(|owned| !owned.is_empty());
        let mut state = self.lock();
        state.shutdown_active = false;
        state.teardown_harness = false;
        state.teardown_retiring = false;
        state.teardown_managed = false;
        state.retiring = owned_sets;
        if first_error.is_some() {
            state.recoverable_error = Some(RECOVERABLE_SHUTDOWN.into());
            Err(ControllerError::ShutdownFailed)
        } else {
            state.recoverable_error = None;
            Ok(())
        }
    }

    #[cfg(test)]
    fn for_test(key_persistence_available: bool, stop_grace: Duration) -> Self {
        Self {
            state: Mutex::new(ControllerState::default()),
            key_persistence_available,
            stop_grace,
            shutdown_budget: stop_grace.saturating_mul(4) + Duration::from_millis(250),
        }
    }
}

fn query_owned_processes(
    owned: &mut OwnedProcesses,
    running: &mut bool,
    query_error: &mut bool,
) {
    for tree in [&mut owned.harness, &mut owned.managed_durindoor] {
        match tree.as_mut().map(|tree| tree.is_running()) {
            Some(Ok(true)) => *running = true,
            Some(Ok(false)) => *tree = None,
            Some(Err(_)) => {
                *running = true;
                *query_error = true;
            }
            None => {}
        }
    }
}


fn stop_tree_until(
    tree: &mut Option<Box<dyn OwnedTree>>,
    grace: Duration,
    deadline: std::time::Instant,
) -> Option<io::Error> {
    if tree.is_some() && std::time::Instant::now() >= deadline {
        return Some(io::Error::new(io::ErrorKind::TimedOut, "controller shutdown budget exhausted"));
    }
    stop_tree(tree, grace)
}

fn stop_tree(tree: &mut Option<Box<dyn OwnedTree>>, grace: Duration) -> Option<io::Error> {
    let Some(owned) = tree.as_mut() else { return None };
    let stop_error = owned.stop(grace).err();
    match owned.is_running() {
        Ok(false) => {
            *tree = None;
            stop_error
        }
        Ok(true) => stop_error.or_else(|| Some(io::Error::new(io::ErrorKind::TimedOut, "owned process tree remains running"))),
        Err(error) => stop_error.or(Some(error)),
    }
}

fn stop_and_prune(owned: &mut OwnedProcesses, grace: Duration) -> Option<io::Error> {
    let mut first_error = stop_tree(&mut owned.harness, grace);
    if let Some(error) = stop_tree(&mut owned.managed_durindoor, grace) {
        first_error.get_or_insert(error);
    }
    first_error
}

fn stop_and_prune_until(
    owned: &mut OwnedProcesses,
    grace: Duration,
    deadline: std::time::Instant,
) -> Option<io::Error> {
    let mut first_error = stop_tree_until(&mut owned.harness, grace, deadline);
    if let Some(error) = stop_tree_until(&mut owned.managed_durindoor, grace, deadline) {
        first_error.get_or_insert(error);
    }
    first_error
}

fn retain_cleanup_survivors(
    controller: &ConnectionController,
    attempted: &mut OwnedProcesses,
) -> Option<io::Error> {
    let mut detached = std::mem::take(attempted);
    {
        let mut state = controller.lock();
        state.retirement_active = true;
        state.teardown_harness |= detached.harness.is_some();
        state.teardown_managed |= detached.managed_durindoor.is_some();
    }
    let error = stop_and_prune(&mut detached, controller.stop_grace);
    let mut state = controller.lock();
    if !detached.is_empty() {
        state.retiring.push(detached);
    }
    state.retirement_active = false;
    state.teardown_harness = false;
    state.teardown_managed = false;
    if error.is_some() {
        state.recoverable_error = Some(RECOVERABLE_CLEANUP.into());
    }
    error
}



#[derive(Debug)]
struct Operation<'a> {
    controller: &'a ConnectionController,
    active: bool,
}

impl Operation<'_> {
    fn adopt(mut self, mut attempted: OwnedProcesses, succeeded: bool) -> Result<(), ControllerError> {
        if !succeeded {
            let _ = retain_cleanup_survivors(self.controller, &mut attempted);
            return Err(ControllerError::ActivationFailed);
        }
        let mut replaced = {
            let mut state = self.controller.lock();
            if state.shutting_down {
                drop(state);
                let _ = retain_cleanup_survivors(self.controller, &mut attempted);
                return Err(ControllerError::ShuttingDown);
            }
            let replaced = std::mem::replace(&mut state.owned, attempted);
            state.retirement_active = !replaced.is_empty();
            state.teardown_harness = replaced.harness.is_some();
            state.teardown_managed = replaced.managed_durindoor.is_some();
            state.teardown_retiring = !replaced.is_empty();
            replaced
        };
        let error = stop_and_prune(&mut replaced, self.controller.stop_grace);
        let mut state = self.controller.lock();
        let shutting_down = state.shutting_down;
        if error.is_some() {
            state.recoverable_error = Some(RECOVERABLE_CLEANUP.into());
        }
        if !replaced.is_empty() {
            state.retiring.push(replaced);
        }
        state.retirement_active = false;
        state.teardown_harness = false;
        state.teardown_managed = false;
        state.teardown_retiring = false;
        state.mutating = false;
        self.active = false;
        if shutting_down {
            Err(ControllerError::ShuttingDown)
        } else {
            Ok(())
        }
    }
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.controller.lock().mutating = false;
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerStatus {
    pub key_persistence_available: bool,
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recoverable_error: Option<String>,
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
    ActivationFailed,
    Busy,
    ShutdownFailed,
    ShuttingDown,
    CleanupPending,
}

impl Serialize for ControllerError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let (code, message) = match self {
            Self::ActivationFailed => ("activation-failed", "Connection activation failed"),
            Self::ApiKeyRequired => ("auth-required", "API key required"),
            Self::Busy => ("busy", "Another connection operation is already running"),
            Self::AuthProbeFailed => ("auth-unverified", "API key could not be verified"),
            Self::EmptyModels => ("empty-models", "DurinDoor returned no models"),
            Self::CleanupPending => ("cleanup-pending", "Started process cleanup is pending"),
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
            Self::ShutdownFailed => ("shutdown-failed", "Owned process shutdown failed"),
            Self::ShuttingDown => ("shutting-down", "Melon is shutting down"),
        };
        let mut state = serializer.serialize_struct("ControllerError", 2)?;
        state.serialize_field("code", code)?;
        state.serialize_field("message", message)?;
        state.end()
    }
}

#[tauri::command]
pub fn status(controller: tauri::State<'_, ConnectionController>) -> ControllerStatus {
    controller.status_snapshot()
}

#[tauri::command]
pub async fn probe(
    controller: tauri::State<'_, ConnectionController>,
    input: ProbeInput,
) -> Result<ProbeResult, ControllerError> {
    let _operation = controller.begin_operation()?;
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
    controller: tauri::State<'_, ConnectionController>,
    probe: Value,
    model: String,
) -> Result<SavedConnection, ControllerError> {
    let _operation = controller.begin_operation()?;
    let _ = (probe, model);
    Err(ControllerError::NotImplemented("connection activation is not available yet".into()))
}

#[tauri::command]
pub fn shutdown(controller: tauri::State<'_, ConnectionController>) -> Result<(), ControllerError> {
    controller.shutdown()
}
#[cfg(test)]
mod tests {
    use super::{
        AuthStatus, ControllerError, HealthStatus, ProbeInput, ProbeLimits, probe_external,
    };
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
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
        address: SocketAddr,
        port: u16,
        requests: Arc<Mutex<Vec<(String, Option<String>)>>>,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn start(health: Reply, auth: Reply, models: Reply) -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind fixture");
            listener.set_nonblocking(true).expect("nonblocking fixture");
            let address = listener.local_addr().expect("fixture address");
            let port = address.port();
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
            Self { address, port, requests, stop, thread: Some(thread) }
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
    fn fixtures_bind_loopback_only() {
        let server = Server::start(
            Reply::json(r#"{"ok":true}"#),
            Reply::status(200),
            Reply::json(r#"{"data":[{"id":"model-a"}]}"#),
        );
        assert_eq!(server.address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn blocks_insecure_non_loopback_http_before_request() {
        let server = Server::start(
            Reply::json(r#"{"ok":true}"#),
            Reply::status(200),
            Reply::json(r#"{"data":[{"id":"model-a"}]}"#),
        );
        let result = run(&input(server.url("192.0.2.1", ""), None, false));
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

#[cfg(test)]
mod ownership_tests {
    use super::{ConnectionController, ControllerError, OwnedProcesses, OwnedTree};
    use crate::process_tree::ProcessTree;
    use std::collections::VecDeque;
    use std::fs;
    use std::io;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct FakeState {
        running: bool,
        stops: usize,
        stop_results: VecDeque<io::Result<()>>,
        running_results: VecDeque<io::Result<bool>>,
    }

    struct FakeTree(Arc<Mutex<FakeState>>);

    impl FakeTree {
        fn running() -> (Box<dyn OwnedTree>, Arc<Mutex<FakeState>>) {
            let state = Arc::new(Mutex::new(FakeState { running: true, ..Default::default() }));
            (Box::new(Self(Arc::clone(&state))), state)
        }

        fn with_results(results: Vec<io::Result<()>>) -> (Box<dyn OwnedTree>, Arc<Mutex<FakeState>>) {
            let state = Arc::new(Mutex::new(FakeState {
                running: true,
                stop_results: results.into(),
                ..Default::default()
            }));
            (Box::new(Self(Arc::clone(&state))), state)
        }
    }

    impl OwnedTree for FakeTree {
        fn is_running(&mut self) -> io::Result<bool> {
            let mut state = self.0.lock().expect("fake state");
            let fallback = state.running;
            state.running_results.pop_front().unwrap_or(Ok(fallback))
        }

        fn stop(&mut self, _grace: Duration) -> io::Result<()> {
            let mut state = self.0.lock().expect("fake state");
            state.stops += 1;
            let result = state.stop_results.pop_front().unwrap_or(Ok(()));
            if result.is_ok() {
                state.running = false;
            }
            result
        }
    }

    fn processes(harness: Option<Box<dyn OwnedTree>>, managed: Option<Box<dyn OwnedTree>>) -> OwnedProcesses {
        OwnedProcesses { harness, managed_durindoor: managed }
    }

    #[test]
    fn serializes_mutations_without_duplicate_execution() {
        let controller = Arc::new(ConnectionController::for_test(true, Duration::from_millis(50)));
        let first = controller.begin_operation().expect("first operation");
        assert_eq!(controller.begin_operation().unwrap_err(), ControllerError::Busy);
        drop(first);
        assert!(controller.begin_operation().is_ok());

        let executions = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(3));
        let results = Arc::new(Mutex::new(Vec::new()));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let controller = Arc::clone(&controller);
            let executions = Arc::clone(&executions);
            let barrier = Arc::clone(&barrier);
            let results = Arc::clone(&results);
            threads.push(thread::spawn(move || {
                barrier.wait();
                let result = controller.run_serialized(|| {
                    executions.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(40));
                });
                results.lock().expect("results").push(result);
            }));
        }
        barrier.wait();
        for thread in threads { thread.join().expect("operation thread"); }
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(results.lock().expect("results").iter().filter(|result| **result == Err(ControllerError::Busy)).count(), 1);
    }

    #[test]
    fn status_derives_from_live_tracked_handles_and_key_availability() {
        let controller = ConnectionController::for_test(true, Duration::from_millis(50));
        let (tree, state) = FakeTree::running();
        let operation = controller.begin_operation().expect("operation");
        operation.adopt(processes(Some(tree), None), true).expect("adopt tree");
        assert_eq!(controller.status_snapshot().key_persistence_available, true);
        assert!(controller.status_snapshot().running);
        state.lock().expect("fake state").running = false;
        assert!(!controller.status_snapshot().running);
    }

    #[test]
    fn no_handle_shutdown_is_idempotent() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(controller.begin_operation().unwrap_err(), ControllerError::ShuttingDown);
    }
    #[test]
    fn status_query_error_is_redacted_conservative_and_retained() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(20));
        let (tree, state) = FakeTree::running();
        state.lock().expect("state").running_results.push_back(Err(io::Error::other("RAW-SENTINEL")));
        controller.begin_operation().expect("operation").adopt(processes(Some(tree), None), true).expect("adopt");

        let status = controller.status_snapshot();
        assert!(status.running);
        assert_eq!(status.recoverable_error.as_deref(), Some(super::RECOVERABLE_QUERY));
        assert!(!serde_json::to_string(&status).expect("status JSON").contains("RAW-SENTINEL"));
        assert!(!controller.lock().owned.is_empty());
    }

    #[test]
    fn stop_and_query_errors_retain_first_and_eventually_clear() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(20));
        let (tree, state) = FakeTree::with_results(vec![
            Err(io::Error::other("RAW-FIRST-STOP")),
            Ok(()),
            Ok(()),
        ]);
        {
            let mut state = state.lock().expect("state");
            state.running_results.extend([
                Err(io::Error::other("RAW-QUERY-ONE")),
                Err(io::Error::other("RAW-QUERY-TWO")),
                Ok(false),
            ]);
        }
        controller.begin_operation().expect("operation").adopt(processes(Some(tree), None), true).expect("adopt");

        assert_eq!(controller.shutdown(), Err(ControllerError::ShutdownFailed));
        let status = controller.status_snapshot();
        assert!(status.running);
        let json = serde_json::to_string(&status).expect("status JSON");
        assert_eq!(status.recoverable_error.as_deref(), Some(super::RECOVERABLE_SHUTDOWN));
        assert!(!json.contains("RAW-FIRST-STOP") && !json.contains("RAW-QUERY"));
        assert_eq!(controller.shutdown(), Ok(()));
        assert!(!controller.status_snapshot().running);
    }

    #[test]
    fn cleanup_pending_blocks_new_mutation_and_bounds_owned_tree_count() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(20));
        let (attempt, _state) = FakeTree::with_results(vec![Err(io::Error::other("cleanup fail"))]);
        assert_eq!(
            controller.begin_operation().expect("operation").adopt(processes(Some(attempt), None), false),
            Err(ControllerError::ActivationFailed),
        );
        assert_eq!(controller.begin_operation().unwrap_err(), ControllerError::CleanupPending);
        let state = controller.lock();
        let count = usize::from(state.owned.harness.is_some())
            + usize::from(state.owned.managed_durindoor.is_some())
            + state.retiring.iter().map(|owned| usize::from(owned.harness.is_some()) + usize::from(owned.managed_durindoor.is_some())).sum::<usize>();
        assert!(count <= super::MAX_OWNED_TREES as usize);
    }

    #[test]
    fn shutdown_blocks_new_operations_and_late_adoption_rolls_back_attempt() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        let operation = controller.begin_operation().expect("operation");
        assert_eq!(controller.shutdown(), Err(ControllerError::ShutdownFailed));
        let (attempt, attempt_state) = FakeTree::running();
        assert_eq!(
            operation.adopt(processes(Some(attempt), None), true),
            Err(ControllerError::ShuttingDown),
        );
        assert_eq!(attempt_state.lock().expect("attempt").stops, 1);
        assert_eq!(controller.begin_operation().unwrap_err(), ControllerError::ShuttingDown);
    }


    #[test]
    fn two_handle_shutdown_attempts_both_and_retries_only_live_failure() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        let (harness, harness_state) = FakeTree::with_results(vec![Err(io::Error::other("first failure")), Ok(())]);
        let (managed, managed_state) = FakeTree::running();
        controller
            .begin_operation().expect("operation")
            .adopt(processes(Some(harness), Some(managed)), true).expect("adopt trees");

        assert_eq!(controller.shutdown(), Err(ControllerError::ShutdownFailed));
        assert_eq!(harness_state.lock().expect("harness").stops, 1);
        assert_eq!(managed_state.lock().expect("managed").stops, 1);
        assert!(controller.status_snapshot().running);

        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(harness_state.lock().expect("harness").stops, 2);
        assert_eq!(managed_state.lock().expect("managed").stops, 1);
        assert!(!controller.status_snapshot().running);
    }

    #[test]
    fn failed_adoption_stops_attempt_and_preserves_prior_tree() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        let (prior, prior_state) = FakeTree::running();
        controller
            .begin_operation().expect("prior operation")
            .adopt(processes(Some(prior), None), true).expect("adopt prior");
        let (attempt, attempt_state) = FakeTree::running();
        let result = controller
            .begin_operation().expect("attempt operation")
            .adopt(processes(Some(attempt), None), false);
        assert_eq!(result, Err(ControllerError::ActivationFailed));
        assert_eq!(attempt_state.lock().expect("attempt").stops, 1);
        assert_eq!(prior_state.lock().expect("prior").stops, 0);
        assert!(controller.status_snapshot().running);
    }
    #[test]
    fn failed_attempted_adoption_tracks_survivor_until_later_shutdown_succeeds() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        let (attempt, state) = FakeTree::with_results(vec![
            Err(io::Error::other("rollback failure one")),
            Err(io::Error::other("rollback failure two")),
            Ok(()),
        ]);

        assert_eq!(
            controller.begin_operation().expect("operation").adopt(processes(Some(attempt), None), false),
            Err(ControllerError::ActivationFailed),
        );
        assert_eq!(state.lock().expect("attempt").stops, 1);
        assert!(controller.status_snapshot().running);
        assert_eq!(controller.shutdown(), Err(ControllerError::ShutdownFailed));
        assert_eq!(state.lock().expect("attempt").stops, 2);
        assert!(controller.status_snapshot().running);
        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(state.lock().expect("attempt").stops, 3);
        assert!(!controller.status_snapshot().running);
    }

    #[test]
    fn successful_replacement_retains_and_retries_every_failed_prior_tree() {
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        let (old_harness, old_harness_state) = FakeTree::with_results(vec![
            Err(io::Error::other("old harness replacement failure")),
            Ok(()),
        ]);
        let (old_managed, old_managed_state) = FakeTree::with_results(vec![
            Err(io::Error::other("old managed replacement failure")),
            Ok(()),
        ]);

        controller
            .begin_operation().expect("old operation")
            .adopt(processes(Some(old_harness), Some(old_managed)), true).expect("adopt old trees");

        let (new_harness, new_harness_state) = FakeTree::running();
        let (new_managed, new_managed_state) = FakeTree::running();
        controller
            .begin_operation().expect("replacement operation")
            .adopt(processes(Some(new_harness), Some(new_managed)), true).expect("adopt new trees");

        assert!(controller.status_snapshot().running);
        assert_eq!(old_harness_state.lock().expect("old harness").stops, 1);
        assert_eq!(old_managed_state.lock().expect("old managed").stops, 1);
        assert_eq!(new_harness_state.lock().expect("new harness").stops, 0);
        assert_eq!(new_managed_state.lock().expect("new managed").stops, 0);

        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(old_harness_state.lock().expect("old harness").stops, 2);
        assert_eq!(old_managed_state.lock().expect("old managed").stops, 2);
        assert_eq!(new_harness_state.lock().expect("new harness").stops, 1);
        assert_eq!(new_managed_state.lock().expect("new managed").stops, 1);
        assert!(!controller.status_snapshot().running);
    }

    struct BlockingFailureTree {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        state: Arc<Mutex<FakeState>>,
    }

    impl OwnedTree for BlockingFailureTree {
        fn is_running(&mut self) -> io::Result<bool> {
            Ok(self.state.lock().expect("blocking failure state").running)
        }

        fn stop(&mut self, _grace: Duration) -> io::Result<()> {
            let mut state = self.state.lock().expect("blocking failure state");
            state.stops += 1;
            let result = state.stop_results.pop_front().unwrap_or(Ok(()));
            let stop = state.stops;
            if result.is_ok() {
                state.running = false;
            }
            drop(state);
            if stop == 1 {
                self.entered.wait();
                self.release.wait();
            }
            result
        }
    }

    #[test]
    fn status_reports_detached_old_tree_during_blocked_empty_replacement() {
        let controller = Arc::new(ConnectionController::for_test(false, Duration::from_millis(100)));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let old_state = Arc::new(Mutex::new(FakeState {
            running: true,
            stop_results: vec![Err(io::Error::other("blocked replacement failure")), Ok(())].into(),
            ..Default::default()
        }));
        controller
            .begin_operation().expect("old operation")
            .adopt(
                processes(Some(Box::new(BlockingFailureTree {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                    state: Arc::clone(&old_state),
                })), None),
                true,
            )
            .expect("adopt old tree");

        let adopting_controller = Arc::clone(&controller);
        let adoption = thread::spawn(move || {
            adopting_controller
                .begin_operation().expect("replacement operation")
                .adopt(processes(None, None), true)
        });
        entered.wait();

        assert!(controller.lock().owned.is_empty(), "attempted ownership is empty");
        let started = Instant::now();
        assert!(controller.status_snapshot().running, "detached old live tree remains visible");
        assert!(started.elapsed() < Duration::from_millis(100), "status does not wait for teardown");
        release.wait();

        assert_eq!(adoption.join().expect("adoption thread"), Ok(()));
        assert!(controller.status_snapshot().running, "failed old tree remains retiring");
        assert_eq!(controller.shutdown(), Ok(()));
        assert!(!controller.status_snapshot().running);
    }
    /// Verifies that when a successful replacement adopts with an old tree live,
    /// the status correctly reports `running=true` and any concurrent mutation is
    /// rejected with `CleanupPending` while the old tree is being torn down.
    #[test]
    fn successful_replacement_with_old_live_blocks_mutation_during_teardown() {
        let controller = Arc::new(ConnectionController::for_test(false, Duration::from_millis(50)));
        let old_entered = Arc::new(Barrier::new(2));
        let old_release = Arc::new(Barrier::new(2));
        let old_state = Arc::new(Mutex::new(FakeState {
            running: true,
            stop_results: vec![
                Err(io::Error::other("old teardown blocked")),
                Ok(()),
            ].into(),
            ..Default::default()
        }));
        controller
            .begin_operation().expect("old operation")
            .adopt(
                processes(Some(Box::new(BlockingFailureTree {
                    entered: Arc::clone(&old_entered),
                    release: Arc::clone(&old_release),
                    state: Arc::clone(&old_state),
                })), None),
                true,
            )
            .expect("adopt old tree");

        let (new_tree, _new_state) = FakeTree::running();
        let adopting_controller = Arc::clone(&controller);
        let adoption = thread::spawn(move || {
            adopting_controller
                .begin_operation().expect("replacement operation")
                .adopt(processes(Some(new_tree), None), true)
        });
        old_entered.wait();

        // Status must report running while the old tree is being torn down.
        let started = Instant::now();
        assert!(controller.status_snapshot().running, "status reports running during teardown");
        assert!(started.elapsed() < Duration::from_millis(100), "status is instantaneous");

        // Any new mutation attempt must be blocked while teardown is in progress.
        drop(controller.begin_operation().expect_err("must be an error"));
        let result = controller.begin_operation().expect_err("must be an error");
        assert_eq!(result, ControllerError::CleanupPending, "new mutation rejected during teardown");

        old_release.wait();
        assert_eq!(adoption.join().expect("adoption thread"), Ok(()));
        assert!(controller.status_snapshot().running, "new tree is running after adoption");
        assert_eq!(controller.shutdown(), Ok(()));
        assert!(!controller.status_snapshot().running);
    }

    #[test]
    fn shutdown_drains_replacement_teardown_that_started_before_shutdown() {
        let controller = Arc::new(ConnectionController::for_test(false, Duration::from_millis(100)));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let old_state = Arc::new(Mutex::new(FakeState {
            running: true,
            stop_results: vec![
                Err(io::Error::other("blocked replacement failure")),
                Err(io::Error::other("shutdown retry failure")),
                Ok(()),
            ].into(),
            ..Default::default()
        }));
        controller
            .begin_operation().expect("old operation")
            .adopt(
                processes(Some(Box::new(BlockingFailureTree {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                    state: Arc::clone(&old_state),
                })), None),
                true,
            )
            .expect("adopt old tree");

        let (new_tree, new_state) = FakeTree::running();
        let adopting_controller = Arc::clone(&controller);
        let adoption = thread::spawn(move || {
            adopting_controller
                .begin_operation().expect("replacement operation")
                .adopt(processes(Some(new_tree), None), true)
        });
        entered.wait();
        let shutdown_controller = Arc::clone(&controller);
        let shutdown = thread::spawn(move || shutdown_controller.shutdown());
        let deadline = Instant::now() + Duration::from_secs(1);
        while !controller.lock().shutting_down && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        let shutdown_started = controller.lock().shutting_down;
        let shutdown_waiting = !shutdown.is_finished();
        release.wait();
        assert!(shutdown_started, "shutdown entered before replacement release");
        assert!(shutdown_waiting, "shutdown waited for active replacement teardown");

        assert_eq!(adoption.join().expect("adoption thread"), Err(ControllerError::ShuttingDown));
        assert_eq!(shutdown.join().expect("shutdown thread"), Err(ControllerError::ShutdownFailed));
        assert_eq!(old_state.lock().expect("old state").stops, 2);
        assert!(old_state.lock().expect("old state").running);
        assert_eq!(new_state.lock().expect("new state").stops, 1);
        assert!(!new_state.lock().expect("new state").running);
        assert!(controller.status_snapshot().running);
        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(old_state.lock().expect("old state").stops, 3);
        assert!(!old_state.lock().expect("old state").running);
        assert!(!controller.status_snapshot().running);
        let state = controller.lock();
        assert!(state.owned.is_empty());
        assert!(state.retiring.is_empty());
        assert!(!state.teardown_harness && !state.teardown_managed && !state.teardown_retiring);
    }

    struct BlockingTree {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        running: Arc<AtomicBool>,
    }

    impl OwnedTree for BlockingTree {
        fn is_running(&mut self) -> io::Result<bool> { Ok(self.running.load(Ordering::SeqCst)) }
        fn stop(&mut self, _grace: Duration) -> io::Result<()> {
            self.entered.wait();
            self.release.wait();
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn status_does_not_wait_for_blocking_teardown_lock() {
        let controller = Arc::new(ConnectionController::for_test(false, Duration::from_millis(50)));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let running = Arc::new(AtomicBool::new(true));
        controller
            .begin_operation().expect("operation")
            .adopt(
                processes(Some(Box::new(BlockingTree {
                    entered: Arc::clone(&entered), release: Arc::clone(&release), running,
                })), None),
                true,
            )
            .expect("adopt blocking tree");
        let shutdown_controller = Arc::clone(&controller);
        let shutdown = thread::spawn(move || shutdown_controller.shutdown());
        entered.wait();
        let started = Instant::now();
        assert!(controller.status_snapshot().running);
        assert!(started.elapsed() < Duration::from_millis(100));
        release.wait();
        assert_eq!(shutdown.join().expect("shutdown thread"), Ok(()));
    }

    #[cfg(unix)]
    fn process_running(pid: u32) -> bool {
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        };
        !matches!(stat.rsplit_once(") ").and_then(|(_, fields)| fields.chars().next()), Some('Z' | 'X'))
    }

    #[cfg(unix)]
    #[test]
    fn tracked_shutdown_stops_real_parent_and_grandchild_only() {
        let ready = std::env::temp_dir().join(format!(
            "melon-controller-tree-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos(),
        ));
        let script = format!("sleep 60 & echo $! > '{}'; wait", ready.display());
        let mut command = Command::new("sh");
        command.args(["-c", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let tree = ProcessTree::spawn(&mut command).expect("spawn real tree");
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            if let Ok(text) = fs::read_to_string(&ready) {
                break text.trim().parse::<u32>().expect("grandchild pid");
            }
            assert!(Instant::now() < deadline, "grandchild readiness timeout");
            thread::sleep(Duration::from_millis(10));
        };
        let controller = ConnectionController::for_test(false, Duration::from_millis(50));
        controller
            .begin_operation().expect("operation")
            .adopt(processes(Some(Box::new(tree)), None), true).expect("adopt real tree");
        assert_eq!(controller.shutdown(), Ok(()));
        assert!(!process_running(grandchild));
        fs::remove_file(ready).expect("remove readiness");
    }
}
