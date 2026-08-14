use url::{Host, Url};

const CREDENTIAL_SERVICE: &str = "com.bloodf.melon";

/// Secure credential operations keyed by a deterministic endpoint account.
pub trait CredentialBackend {
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    fn get(&self, account: &str) -> Result<String, String>;
}

/// OS-native credential storage for Melon API keys.
pub struct NativeCredentialBackend;

impl CredentialBackend for NativeCredentialBackend {
    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        keyring::Entry::new(CREDENTIAL_SERVICE, account)
            .and_then(|entry| entry.set_password(secret))
            .map_err(|error| error.to_string())
    }

    fn get(&self, account: &str) -> Result<String, String> {
        keyring::Entry::new(CREDENTIAL_SERVICE, account)
            .and_then(|entry| entry.get_password())
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Persistence {
    Native,
    SessionOnly,
}

/// Keeps a key in memory when native credential storage is unavailable or locked.
pub struct CredentialStore<B> {
    backend: B,
    session: std::collections::HashMap<String, String>,
}

impl<B: CredentialBackend> CredentialStore<B> {
    pub fn new(backend: B) -> Self {
        Self { backend, session: std::collections::HashMap::new() }
    }

    pub fn save(&mut self, account: &str, secret: &str) -> Persistence {
        if self.backend.set(account, secret).is_ok() {
            self.session.remove(account);
            Persistence::Native
        } else {
            self.session.insert(account.to_owned(), secret.to_owned());
            Persistence::SessionOnly
        }
    }

    pub fn load(&self, account: &str) -> Option<String> {
        self.session.get(account).cloned().or_else(|| self.backend.get(account).ok())
    }
}

impl CredentialStore<NativeCredentialBackend> {
    pub fn native() -> Self {
        Self::new(NativeCredentialBackend)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EndpointError {
    Invalid,
    Scheme,
    Credentials,
    Query,
    Fragment,
    PathAfterV1,
}

#[derive(Debug)]
pub struct NormalizedEndpoint {
    url: Url,
    pub requires_insecure_confirmation: bool,
}

impl NormalizedEndpoint {
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }

    /// Returns stable non-secret keyring account text from this normalized endpoint.
    pub fn credential_account(&self) -> String {
        self.as_str().to_owned()
    }
}

pub fn normalize_endpoint(input: &str) -> Result<NormalizedEndpoint, EndpointError> {
    let mut url = Url::parse(input).map_err(|_| EndpointError::Invalid)?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(EndpointError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::Credentials);
    }
    if url.query().is_some() {
        return Err(EndpointError::Query);
    }
    if url.fragment().is_some() {
        return Err(EndpointError::Fragment);
    }

    let path = url.path().trim_end_matches('/').to_owned();
    let segments: Vec<_> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    if segments.iter().position(|segment| *segment == "v1").is_some_and(|index| index + 1 != segments.len()) {
        return Err(EndpointError::PathAfterV1);
    }
    if path.is_empty() {
        url.set_path("/v1");
    } else if segments.last() == Some(&"v1") {
        url.set_path(&path);
    } else {
        url.set_path(&format!("{path}/v1"));
    }

    let loopback = match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    Ok(NormalizedEndpoint {
        requires_insecure_confirmation: url.scheme() == "http" && !loopback,
        url,
    })
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionMode {
    ManagedLocal,
    External,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ModelRecord {
    pub id: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDocument {
    pub schema_version: u32,
    pub mode: ConnectionMode,
    pub base_url: String,
    pub model: String,
    pub allow_insecure_http: bool,
    pub credential_account: Option<String>,
    pub catalog: Vec<ModelRecord>,
    pub managed_runtime_version: Option<String>,
}

pub fn write_connection(path: &std::path::Path, connection: &ConnectionDocument) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "connection path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "connection path has no file name"))?;
    let temporary = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let bytes = serde_json::to_vec_pretty(connection).map_err(std::io::Error::other)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = replace_file(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &std::path::Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(windows)]
fn sync_parent(_parent: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let source: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<_> = destination.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{EndpointError, normalize_endpoint};

    #[test]
    fn appends_v1_to_an_origin_and_preserves_prefix() {
        assert_eq!(normalize_endpoint("https://example.com").unwrap().as_str(), "https://example.com/v1");
        assert_eq!(normalize_endpoint("https://example.com/prefix").unwrap().as_str(), "https://example.com/prefix/v1");
        assert_eq!(normalize_endpoint("https://example.com/prefix/v1").unwrap().as_str(), "https://example.com/prefix/v1");
    }

    #[test]
    fn rejects_credentials_query_fragment_and_paths_after_v1() {
        assert!(matches!(normalize_endpoint("https://user:pass@example.com"), Err(EndpointError::Credentials)));
        assert!(matches!(normalize_endpoint("https://example.com?key=x"), Err(EndpointError::Query)));
        assert!(matches!(normalize_endpoint("https://example.com#token"), Err(EndpointError::Fragment)));
        assert!(matches!(normalize_endpoint("https://example.com/v1/models"), Err(EndpointError::PathAfterV1)));
    }

    #[test]
    fn rejects_unsupported_schemes_and_requires_http_confirmation_off_loopback() {
        assert!(matches!(normalize_endpoint("file:///tmp/socket"), Err(EndpointError::Scheme)));
        assert!(!normalize_endpoint("http://127.0.0.1:20128").unwrap().requires_insecure_confirmation);
        assert!(!normalize_endpoint("http://[::1]:20128").unwrap().requires_insecure_confirmation);
        assert!(normalize_endpoint("http://192.168.1.10:20128").unwrap().requires_insecure_confirmation);
        assert!(!normalize_endpoint("https://gateway.example").unwrap().requires_insecure_confirmation);
    }

    #[test]
    fn credential_account_uses_normalized_endpoint() {
        let origin = normalize_endpoint("https://EXAMPLE.com/").unwrap();
        let explicit = normalize_endpoint("https://example.com/v1/").unwrap();
        assert_eq!(origin.credential_account(), explicit.credential_account());
        assert_eq!(origin.credential_account(), "https://example.com/v1");
    }

    #[test]
    fn writes_non_secret_connection_atomically() {
        use super::{ConnectionDocument, ConnectionMode, ModelRecord, write_connection};
        let root = std::env::temp_dir().join(format!("melon-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("connection.json");
        let connection = ConnectionDocument {
            schema_version: 1,
            mode: ConnectionMode::External,
            base_url: "https://example.com/v1".into(),
            model: "model-a".into(),
            allow_insecure_http: false,
            credential_account: Some("endpoint-account".into()),
            catalog: vec![ModelRecord { id: "model-a".into() }],
            managed_runtime_version: None,
        };
        write_connection(&path, &connection).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("apiKey"));
        assert_eq!(serde_json::from_str::<ConnectionDocument>(&text).unwrap(), connection);
        let replacement = ConnectionDocument { model: "model-b".into(), ..connection };
        write_connection(&path, &replacement).unwrap();
        assert_eq!(serde_json::from_str::<ConnectionDocument>(&std::fs::read_to_string(&path).unwrap()).unwrap(), replacement);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keyring_failure_uses_session_only_storage() {
        use super::{CredentialBackend, CredentialStore, Persistence};
        struct Locked;
        impl CredentialBackend for Locked {
            fn set(&self, _account: &str, _secret: &str) -> Result<(), String> { Err("locked".into()) }
            fn get(&self, _account: &str) -> Result<String, String> { Err("locked".into()) }
        }
        let mut store = CredentialStore::new(Locked);
        assert_eq!(store.save("endpoint", "secret"), Persistence::SessionOnly);
        assert_eq!(store.load("endpoint").as_deref(), Some("secret"));
    }

}
