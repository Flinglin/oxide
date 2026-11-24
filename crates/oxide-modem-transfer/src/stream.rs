use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::error::ModemTransferError;
use crate::io::ModemIo;

pub type ModemWakeCallback = Arc<dyn Fn() + Send + Sync + 'static>;

// Keep protocol buffering bounded while allowing several full ZMODEM frames
// to bridge the worker and the terminal transport without blocking the UI.
const MODEM_REMOTE_OUTPUT_BUFFER_BYTES: usize = 4 * 1024 * 1024;
const MODEM_SERVER_WRITE_BUFFER_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct ModemTransfer {
    inner: Arc<ModemTransferInner>,
}

struct ModemTransferInner {
    state: Mutex<ModemTransferState>,
    available: Condvar,
    wake_host: Option<ModemWakeCallback>,
}

#[derive(Debug, Default)]
struct ModemTransferState {
    remote_output: VecDeque<u8>,
    server_writes: VecDeque<Vec<u8>>,
    server_write_bytes: usize,
    server_write_in_flight_bytes: usize,
    cancellation_bytes: Vec<u8>,
    input_overflow: bool,
    stopped: bool,
}

impl ModemTransfer {
    pub fn new(initial_remote_output: &[u8]) -> Self {
        Self::new_with_wake_and_cancel(initial_remote_output, None, &[])
    }

    pub fn new_with_wake(
        initial_remote_output: &[u8],
        wake_host: Option<ModemWakeCallback>,
    ) -> Self {
        Self::new_with_wake_and_cancel(initial_remote_output, wake_host, &[])
    }

    pub fn new_with_wake_and_cancel(
        initial_remote_output: &[u8],
        wake_host: Option<ModemWakeCallback>,
        cancellation_bytes: &[u8],
    ) -> Self {
        let mut state = ModemTransferState::default();
        state.remote_output.extend(initial_remote_output);
        state
            .cancellation_bytes
            .extend_from_slice(cancellation_bytes);
        Self {
            inner: Arc::new(ModemTransferInner {
                state: Mutex::new(state),
                available: Condvar::new(),
                wake_host,
            }),
        }
    }

    pub fn push_remote_output(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut state = self.inner.state.lock().expect("modem transfer state");
        if state.stopped {
            return;
        }
        if state.remote_output.len().saturating_add(bytes.len()) > MODEM_REMOTE_OUTPUT_BUFFER_BYTES
        {
            state.remote_output.clear();
            state.input_overflow = true;
            Self::cancel_locked(&mut state);
            self.inner.available.notify_all();
            drop(state);
            self.wake_host();
            return;
        }
        state.remote_output.extend(bytes);
        self.inner.available.notify_all();
    }

    pub fn take_server_write(&self) -> Option<Vec<u8>> {
        let mut state = self.inner.state.lock().expect("modem transfer state");
        let bytes = state.server_writes.pop_front()?;
        state.server_write_in_flight_bytes = state
            .server_write_in_flight_bytes
            .saturating_add(bytes.len());
        Some(bytes)
    }

    pub fn complete_server_write(&self, byte_len: usize) {
        let mut state = self.inner.state.lock().expect("modem transfer state");
        state.server_write_in_flight_bytes =
            state.server_write_in_flight_bytes.saturating_sub(byte_len);
        state.server_write_bytes = state.server_write_bytes.saturating_sub(byte_len);
        self.inner.available.notify_all();
    }

    pub fn restore_server_write(&self, bytes: Vec<u8>) {
        let mut state = self.inner.state.lock().expect("modem transfer state");
        state.server_write_in_flight_bytes = state
            .server_write_in_flight_bytes
            .saturating_sub(bytes.len());
        state.server_writes.push_front(bytes);
        self.inner.available.notify_all();
    }

    pub fn server_writes_drained(&self) -> bool {
        let state = self.inner.state.lock().expect("modem transfer state");
        state.server_write_bytes == 0 && state.server_write_in_flight_bytes == 0
    }

    pub fn drain_remote_output(&self) -> Vec<u8> {
        let mut state = self.inner.state.lock().expect("modem transfer state");
        state.remote_output.drain(..).collect()
    }

    pub fn stop(&self) {
        let mut state = self.inner.state.lock().expect("modem transfer state");
        Self::cancel_locked(&mut state);
        self.inner.available.notify_all();
        drop(state);
        self.wake_host();
    }

    fn wake_host(&self) {
        if let Some(wake_host) = &self.inner.wake_host {
            wake_host();
        }
    }

    fn cancel_locked(state: &mut ModemTransferState) {
        if state.stopped {
            return;
        }
        state.stopped = true;
        state.remote_output.clear();

        // Unsent data is no longer useful after cancellation. Replace it with
        // the protocol abort sequence so the peer exits instead of retrying.
        let queued_bytes = state.server_writes.iter().map(Vec::len).sum::<usize>();
        state.server_writes.clear();
        state.server_write_bytes = state.server_write_bytes.saturating_sub(queued_bytes);
        if !state.cancellation_bytes.is_empty() {
            let cancellation = state.cancellation_bytes.clone();
            state.server_write_bytes = state.server_write_bytes.saturating_add(cancellation.len());
            state.server_writes.push_back(cancellation);
        }
    }
}

impl fmt::Debug for ModemTransfer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModemTransfer")
            .finish_non_exhaustive()
    }
}

impl ModemIo for ModemTransfer {
    fn read_byte(&mut self, timeout: Duration) -> Result<u8, ModemTransferError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.inner.state.lock().expect("modem transfer state");
        loop {
            if state.input_overflow {
                return Err(ModemTransferError::InputBufferOverflow(
                    MODEM_REMOTE_OUTPUT_BUFFER_BYTES,
                ));
            }
            if state.stopped {
                return Err(ModemTransferError::Cancelled);
            }
            if let Some(byte) = state.remote_output.pop_front() {
                return Ok(byte);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(ModemTransferError::Timeout);
            };
            let (new_state, timeout) = self
                .inner
                .available
                .wait_timeout(state, remaining)
                .expect("modem transfer state");
            state = new_state;
            if timeout.timed_out() {
                return Err(ModemTransferError::Timeout);
            }
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), ModemTransferError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut state = self.inner.state.lock().expect("modem transfer state");
        loop {
            if state.stopped {
                return Err(ModemTransferError::Cancelled);
            }
            let buffered_after_write = state.server_write_bytes.saturating_add(bytes.len());
            let oversized_first_write =
                state.server_write_bytes == 0 && state.server_write_in_flight_bytes == 0;
            if buffered_after_write <= MODEM_SERVER_WRITE_BUFFER_BYTES || oversized_first_write {
                break;
            }
            state = self
                .inner
                .available
                .wait(state)
                .expect("modem transfer state");
        }
        state.server_write_bytes = state.server_write_bytes.saturating_add(bytes.len());
        state.server_writes.push_back(bytes.to_vec());
        drop(state);
        self.wake_host();
        Ok(())
    }
}
