use super::management::ManagementOperation;
use crate::{
    AuditError, AuditExportFormat, AuditPage, AuditQuery, AuditSessionPage, AuditSessionQuery,
    AuditStore,
};
use async_channel::{Sender, TrySendError};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

// A slow reader cannot consume the event queue or grow an unbounded backlog.
const READ_QUEUE_CAPACITY: usize = 8;

pub(super) enum ReadRequest {
    Query(AuditQuery, Sender<Result<AuditPage, AuditError>>),
    Sessions(
        AuditSessionQuery,
        Sender<Result<AuditSessionPage, AuditError>>,
    ),
    RecordingList(
        Option<String>,
        Option<(i64, String)>,
        usize,
        Sender<Result<crate::RecordingListPage, AuditError>>,
    ),
    RecordingPage(
        String,
        Option<i64>,
        Option<i64>,
        usize,
        Sender<Result<crate::RecordingPage, AuditError>>,
    ),
    Export(
        AuditQuery,
        PathBuf,
        AuditExportFormat,
        bool,
        ManagementOperation,
        Sender<Result<u64, AuditError>>,
    ),
}

impl ReadRequest {
    pub(super) fn fail(self, error: AuditError) {
        match self {
            Self::RecordingList(_, _, _, sender) => {
                let _ = sender.try_send(Err(error));
            }
            Self::RecordingPage(_, _, _, _, sender) => {
                let _ = sender.try_send(Err(error));
            }
            Self::Query(_, sender) => {
                let _ = sender.try_send(Err(error));
            }
            Self::Sessions(_, sender) => {
                let _ = sender.try_send(Err(error));
            }
            Self::Export(_, _, _, _, operation, sender) => {
                operation.export_completed(Err(error), sender);
            }
        }
    }
}

pub(super) struct AuditReader {
    sender: Sender<ReadRequest>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AuditReader {
    pub(super) fn start(store: AuditStore) -> Result<Self, AuditError> {
        let (sender, receiver) = async_channel::bounded::<ReadRequest>(READ_QUEUE_CAPACITY);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let worker = std::thread::Builder::new()
            .name("audit-reader".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv_blocking() {
                    if stop.load(Ordering::Acquire) {
                        request.fail(AuditError::Closed);
                        continue;
                    }
                    match request {
                        ReadRequest::RecordingList(session_id, before, limit, sender) => {
                            let result = match session_id {
                                Some(id) => store.list_recordings_for_session(&id, before, limit),
                                None => store.list_recordings(before, limit),
                            };
                            let _ = sender.try_send(result);
                        }
                        ReadRequest::RecordingPage(id, after_sequence, after_ms, limit, sender) => {
                            let _ = sender.try_send(store.read_recording_page(
                                &id,
                                after_sequence,
                                after_ms,
                                limit,
                            ));
                        }
                        ReadRequest::Query(query, sender) => {
                            let cancelled = || stop.load(Ordering::Acquire) || sender.is_closed();
                            let result = store.query_until_cancelled(&query, &cancelled);
                            let _ = sender.try_send(result);
                        }
                        ReadRequest::Sessions(query, sender) => {
                            let cancelled = || stop.load(Ordering::Acquire) || sender.is_closed();
                            let result = store.list_sessions_until_cancelled(&query, &cancelled);
                            let _ = sender.try_send(result);
                        }
                        ReadRequest::Export(query, path, format, details, operation, sender) => {
                            let cancelled = || stop.load(Ordering::Acquire) || sender.is_closed();
                            let result =
                                export_file(&store, &query, path, format, details, &cancelled);
                            operation.export_completed(result, sender);
                        }
                    }
                }
            })
            .map_err(|_| AuditError::Storage)?;
        Ok(Self {
            sender,
            stopping,
            worker: Some(worker),
        })
    }

    pub(super) fn send(&self, request: ReadRequest) -> Result<(), Box<(ReadRequest, AuditError)>> {
        self.sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(request) => Box::new((request, AuditError::QueueFull)),
            TrySendError::Closed(request) => Box::new((request, AuditError::Closed)),
        })
    }
}

impl Drop for AuditReader {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.sender.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn export_file(
    store: &AuditStore,
    query: &AuditQuery,
    path: PathBuf,
    format: AuditExportFormat,
    details: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<u64, AuditError> {
    if cancelled() {
        return Err(AuditError::Closed);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let temporary = path.with_file_name(format!(".oxide-audit-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = options.open(&temporary).map_err(|_| AuditError::Storage)?;
    let result = store
        .export_until_cancelled(query, format, details, &mut file, cancelled)
        .and_then(|count| {
            file.sync_all()
                .map(|_| count)
                .map_err(|_| AuditError::Storage)
        });
    drop(file);
    let result = result.and_then(|count| {
        if cancelled() {
            return Err(AuditError::Closed);
        }
        // Replacement is the commit point; cancellation leaves the prior destination intact.
        oxide_atomic_file::durable_replace(&temporary, &path)
            .map(|_| count)
            .map_err(|_| AuditError::Storage)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
