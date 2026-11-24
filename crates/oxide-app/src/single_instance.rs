use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use fs2::FileExt;
use oxide_settings::is_prerelease_version;
use oxide_ssh_launch::{NativeConnectionHandoff, NativeConnectionLaunch};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;
use zeroize::Zeroizing;

const INSTANCE_FILENAME_PREFIX: &str = "oxide-native-instance";
const FORWARD_RETRY_COUNT: usize = 40;
const FORWARD_RETRY_DELAY: Duration = Duration::from_millis(50);
const MAX_INSTANCE_REQUEST_BYTES: u64 = 64 * 1024;

// The application keeps this shared receiver alive while individual workspace
// windows attach and detach from the single-instance event stream.
#[derive(Clone)]
pub(crate) struct SingleInstanceReceiver {
    receiver: Arc<Mutex<mpsc::Receiver<SingleInstanceEvent>>>,
    sender: mpsc::Sender<SingleInstanceEvent>,
    notification: Arc<Notify>,
}

impl SingleInstanceReceiver {
    pub(crate) fn lock(
        &self,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'_, mpsc::Receiver<SingleInstanceEvent>>> {
        self.receiver.lock()
    }

    pub(crate) fn notification(&self) -> Arc<Notify> {
        self.notification.clone()
    }

    pub(crate) fn publish(&self, event: SingleInstanceEvent) -> bool {
        if self.sender.send(event).is_err() {
            return false;
        }
        self.notification.notify_one();
        true
    }
}

pub(crate) enum SingleInstanceOutcome {
    Primary {
        _guard: SingleInstanceGuard,
        receiver: SingleInstanceReceiver,
        startup_launch: Option<NativeConnectionLaunch>,
    },
    Forwarded,
}

#[derive(Debug)]
pub(crate) enum SingleInstanceEvent {
    ShowMainWindow,
    OpenNativeConnection(NativeConnectionHandoff),
    OpenExternalConnectionUri(NativeConnectionLaunch),
}

pub(crate) struct SingleInstanceGuard {
    _lock_file: File,
    state_path: PathBuf,
}

#[derive(Clone, Debug)]
struct InstancePaths {
    lock_path: PathBuf,
    state_path: PathBuf,
}

#[derive(Deserialize)]
struct InstanceState {
    port: u16,
    token: InstanceToken,
}

#[derive(Serialize)]
struct InstanceStateWire<'a> {
    port: u16,
    token: &'a str,
}

#[derive(Deserialize)]
struct InstanceRequest {
    token: InstanceToken,
    #[serde(default, alias = "ssh_launch_file")]
    connection_launch_file: Option<PathBuf>,
    #[serde(default)]
    connection_launch: Option<NativeConnectionLaunch>,
}

#[derive(Serialize)]
struct InstanceRequestWire<'a> {
    token: &'a str,
    connection_launch_file: Option<&'a Path>,
    connection_launch: Option<&'a NativeConnectionLaunch>,
}

#[derive(Deserialize, Eq, PartialEq)]
#[serde(transparent)]
struct InstanceToken(Zeroizing<String>);

impl InstanceToken {
    fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    fn expose_secret(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for InstanceToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted single-instance token]")
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.state_path);
    }
}

impl InstancePaths {
    fn for_data_dir(data_dir: impl Into<PathBuf>, scope: &str) -> Self {
        let data_dir = data_dir.into();
        Self {
            lock_path: data_dir.join(format!("{INSTANCE_FILENAME_PREFIX}-{scope}.lock")),
            state_path: data_dir.join(format!("{INSTANCE_FILENAME_PREFIX}-{scope}.json")),
        }
    }
}

fn instance_scope_for_build(version: &str, development: bool) -> &'static str {
    // Development binaries must coexist with installed channels while each
    // installed channel retains strict single-instance behavior of its own.
    if development {
        return "development";
    }
    if is_prerelease_version(version) {
        return "beta";
    }
    "stable"
}

fn current_instance_scope() -> &'static str {
    instance_scope_for_build(env!("CARGO_PKG_VERSION"), cfg!(debug_assertions))
}

pub(crate) fn single_instance_runtime_paths_for_data_dir(data_dir: &Path) -> [PathBuf; 2] {
    // Startup creates these files before the pre-2.0 snapshot check. Exposing
    // their exact paths prevents current runtime state from looking like 1.x data.
    let paths = InstancePaths::for_data_dir(data_dir, current_instance_scope());
    [paths.lock_path, paths.state_path]
}

pub(crate) fn acquire_or_forward(
    connection_launch_path: Option<PathBuf>,
    connection_launch: Option<NativeConnectionLaunch>,
) -> Result<SingleInstanceOutcome> {
    let settings_path = oxide_settings::default_settings_path();
    let data_dir = settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    acquire_or_forward_with_paths(
        InstancePaths::for_data_dir(data_dir, current_instance_scope()),
        connection_launch_path,
        connection_launch,
    )
}

fn acquire_or_forward_with_paths(
    paths: InstancePaths,
    connection_launch_path: Option<PathBuf>,
    connection_launch: Option<NativeConnectionLaunch>,
) -> Result<SingleInstanceOutcome> {
    let data_dir = paths
        .lock_path
        .parent()
        .ok_or_else(|| anyhow!("single-instance lock path has no parent"))?;
    fs::create_dir_all(data_dir).with_context(|| {
        format!(
            "failed to create single-instance directory {}",
            data_dir.display()
        )
    })?;

    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.lock_path)
        .with_context(|| {
            format!(
                "failed to open single-instance lock {}",
                paths.lock_path.display()
            )
        })?;

    match lock_file.try_lock_exclusive() {
        Ok(()) => start_primary(lock_file, paths, connection_launch),
        // Windows reports ERROR_LOCK_VIOLATION, which is not mapped to WouldBlock.
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            forward_to_primary(&paths.state_path, connection_launch_path, connection_launch)
                .with_context(|| {
                    format!(
                        "failed to forward launch request through {}",
                        paths.state_path.display()
                    )
                })?;
            Ok(SingleInstanceOutcome::Forwarded)
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to acquire single-instance lock {}",
                paths.lock_path.display()
            )
        }),
    }
}

fn start_primary(
    lock_file: File,
    paths: InstancePaths,
    startup_launch: Option<NativeConnectionLaunch>,
) -> Result<SingleInstanceOutcome> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .context("failed to bind single-instance handoff listener")?;
    let port = listener
        .local_addr()
        .context("failed to read single-instance handoff listener address")?
        .port();
    let token = InstanceToken::new(Uuid::new_v4().to_string());
    let state = InstanceStateWire {
        port,
        token: token.expose_secret(),
    };
    let state_bytes = Zeroizing::new(
        serde_json::to_vec(&state).context("failed to encode single-instance state")?,
    );
    fs::write(&paths.state_path, state_bytes.as_slice()).with_context(|| {
        format!(
            "failed to write single-instance state {}",
            paths.state_path.display()
        )
    })?;

    let (tx, rx) = mpsc::channel();
    let listener_tx = tx.clone();
    let notification = Arc::new(Notify::new());
    let listener_notification = notification.clone();
    thread::Builder::new()
        .name("oxide-single-instance".to_string())
        .spawn(move || {
            accept_forwarded_requests(listener, token, listener_tx, listener_notification);
        })
        .context("failed to spawn single-instance handoff listener")?;

    Ok(SingleInstanceOutcome::Primary {
        _guard: SingleInstanceGuard {
            _lock_file: lock_file,
            state_path: paths.state_path,
        },
        receiver: SingleInstanceReceiver {
            receiver: Arc::new(Mutex::new(rx)),
            sender: tx,
            notification,
        },
        startup_launch,
    })
}

fn forward_to_primary(
    state_path: &Path,
    connection_launch_path: Option<PathBuf>,
    connection_launch: Option<NativeConnectionLaunch>,
) -> Result<()> {
    let mut last_error = None;
    for _ in 0..FORWARD_RETRY_COUNT {
        match read_instance_state(state_path).and_then(|state| {
            send_instance_request(
                &state,
                connection_launch_path.as_deref(),
                connection_launch.as_ref(),
            )
        }) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(FORWARD_RETRY_DELAY);
    }

    // If forwarding fails, this process is the only owner of a one-shot CLI
    // handoff file. Remove it so a stdin password is not left behind on disk.
    if let Some(path) = connection_launch_path {
        let _ = fs::remove_file(path);
    }

    Err(last_error.unwrap_or_else(|| anyhow!("single-instance handoff listener was unavailable")))
}

fn read_instance_state(path: &Path) -> Result<InstanceState> {
    let bytes = Zeroizing::new(
        fs::read(path).with_context(|| format!("failed to read {}", path.display()))?,
    );
    serde_json::from_slice(&bytes).context("invalid single-instance state")
}

fn send_instance_request(
    state: &InstanceState,
    connection_launch_path: Option<&Path>,
    connection_launch: Option<&NativeConnectionLaunch>,
) -> Result<()> {
    let mut stream = TcpStream::connect(("127.0.0.1", state.port))
        .context("failed to connect to existing oxide instance")?;
    let request = InstanceRequestWire {
        token: state.token.expose_secret(),
        connection_launch_file: connection_launch_path,
        connection_launch,
    };
    let bytes =
        Zeroizing::new(serde_json::to_vec(&request).context("failed to encode launch request")?);
    stream
        .write_all(&bytes)
        .context("failed to write launch request")
}

fn accept_forwarded_requests(
    listener: TcpListener,
    token: InstanceToken,
    tx: mpsc::Sender<SingleInstanceEvent>,
    notification: Arc<Notify>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        if let Ok(events) = events_from_stream(stream, &token) {
            for event in events {
                if tx.send(event).is_ok() {
                    notification.notify_one();
                }
            }
        }
    }
    // Wake the UI once more so it can observe a disconnected listener.
    notification.notify_one();
}

fn events_from_stream(
    mut stream: TcpStream,
    token: &InstanceToken,
) -> Result<Vec<SingleInstanceEvent>> {
    let mut bytes = Zeroizing::new(Vec::new());
    Read::by_ref(&mut stream)
        .take(MAX_INSTANCE_REQUEST_BYTES)
        .read_to_end(&mut bytes)
        .context("failed to read single-instance request")?;
    let request: InstanceRequest =
        serde_json::from_slice(&bytes).context("invalid single-instance request")?;
    if request.token.expose_secret() != token.expose_secret() {
        return Err(anyhow!("single-instance token mismatch"));
    }

    if request.connection_launch_file.is_some() && request.connection_launch.is_some() {
        return Err(anyhow!(
            "single-instance request has multiple connection launches"
        ));
    }
    let mut events = Vec::new();
    if let Some(path) = request.connection_launch_file {
        events.push(SingleInstanceEvent::ShowMainWindow);
        match read_connection_launch_file(Some(path)) {
            Ok(Some(launch)) => events.push(SingleInstanceEvent::OpenNativeConnection(launch)),
            Ok(None) => {}
            Err(error) => eprintln!("failed to read forwarded connection launch request: {error}"),
        }
    }
    if let Some(launch) = request.connection_launch {
        // Direct in-memory launches originate from operating-system URI
        // callbacks, while explicit CLI requests use the owner-only file path.
        events.push(SingleInstanceEvent::OpenExternalConnectionUri(launch));
    }
    if events.is_empty() {
        events.push(SingleInstanceEvent::ShowMainWindow);
    }
    Ok(events)
}

pub(crate) fn read_connection_launch_file(
    path: Option<PathBuf>,
) -> Result<Option<NativeConnectionHandoff>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let bytes =
        Zeroizing::new(fs::read(&path).with_context(|| {
            format!("failed to read connection launch file {}", path.display())
        })?);
    // The CLI handoff file may contain a stdin password. Delete it only after
    // the owning app instance has accepted the request.
    let _ = fs::remove_file(&path);
    let mut handoff = serde_json::from_slice::<NativeConnectionHandoff>(&bytes)
        .or_else(|_| serde_json::from_slice::<NativeConnectionLaunch>(&bytes).map(Into::into))
        .context("invalid connection launch request")?;
    if handoff
        .audit_parent_id
        .as_deref()
        .is_some_and(|id| Uuid::parse_str(id).is_err())
    {
        handoff.audit_parent_id = None;
    }
    Ok(Some(handoff))
}
