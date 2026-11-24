use std::{io::Write, sync::mpsc, time::Duration};

use crate::{RemoteDesktopHelperRequest, RemoteDesktopJsonLineError, write_request_line};

const REQUEST_DRAIN_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteDesktopRequestBatchOutcome {
    Continue,
    ShutdownRequested,
}

#[derive(Default)]
pub struct RemoteDesktopRequestWriteCoalescer {
    pending_mouse_move: Option<RemoteDesktopHelperRequest>,
}

impl RemoteDesktopRequestWriteCoalescer {
    pub fn push(
        &mut self,
        request: RemoteDesktopHelperRequest,
        output: &mut Vec<RemoteDesktopHelperRequest>,
    ) {
        match request {
            RemoteDesktopHelperRequest::MouseMove { .. } => {
                // Mouse movement is lossy state, while key and button edges are ordered.
                self.pending_mouse_move = Some(request);
            }
            request => {
                self.flush(output);
                output.push(request);
            }
        }
    }

    pub fn flush(&mut self, output: &mut Vec<RemoteDesktopHelperRequest>) {
        if let Some(request) = self.pending_mouse_move.take() {
            output.push(request);
        }
    }
}

pub(crate) fn write_remote_desktop_request_batch(
    writer: &mut impl Write,
    request_rx: &mpsc::Receiver<RemoteDesktopHelperRequest>,
    wait_timeout: Duration,
) -> Result<RemoteDesktopRequestBatchOutcome, RemoteDesktopJsonLineError> {
    let first_request = match request_rx.recv_timeout(wait_timeout) {
        Ok(request) => request,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Ok(RemoteDesktopRequestBatchOutcome::Continue);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Ok(RemoteDesktopRequestBatchOutcome::ShutdownRequested);
        }
    };
    write_remote_desktop_request_batch_from_first(writer, request_rx, first_request)
}

fn write_remote_desktop_request_batch_from_first(
    writer: &mut impl Write,
    request_rx: &mpsc::Receiver<RemoteDesktopHelperRequest>,
    first_request: RemoteDesktopHelperRequest,
) -> Result<RemoteDesktopRequestBatchOutcome, RemoteDesktopJsonLineError> {
    let mut disconnected = false;
    let mut coalescer = RemoteDesktopRequestWriteCoalescer::default();
    let mut requests = Vec::new();
    coalescer.push(first_request, &mut requests);

    for _ in 0..REQUEST_DRAIN_LIMIT {
        match request_rx.try_recv() {
            Ok(request) => coalescer.push(request, &mut requests),
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }
    coalescer.flush(&mut requests);

    for request in requests {
        let should_close = matches!(request, RemoteDesktopHelperRequest::Close);
        write_request_line(writer, &request)?;
        if should_close {
            return Ok(RemoteDesktopRequestBatchOutcome::ShutdownRequested);
        }
    }

    if disconnected {
        Ok(RemoteDesktopRequestBatchOutcome::ShutdownRequested)
    } else {
        Ok(RemoteDesktopRequestBatchOutcome::Continue)
    }
}
