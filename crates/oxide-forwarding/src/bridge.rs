use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use oxide_ssh::BoxedSshForwardStream;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{Notify, mpsc, watch},
};

use crate::{ForwardStats, ForwardingError};

pub const FORWARD_BRIDGE_READ_BUFFER_SIZE: usize = 32 * 1024;
pub const FORWARD_BRIDGE_CHANNEL_CAPACITY: usize = 32;
pub const DEFAULT_FORWARD_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default)]
pub struct ActiveConnectionCounter {
    count: Arc<AtomicU64>,
    notify: Arc<Notify>,
}

impl ActiveConnectionCounter {
    pub fn increment(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    pub fn decrement(&self) {
        self.count
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                Some(count.saturating_sub(1))
            })
            .ok();
        self.notify.notify_waiters();
    }

    pub fn get(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    pub async fn wait_zero(&self, timeout: Duration) -> bool {
        if self.get() == 0 {
            return true;
        }

        tokio::time::timeout(timeout, async {
            while self.get() != 0 {
                self.notify.notified().await;
            }
        })
        .await
        .is_ok()
    }
}

#[derive(Clone, Debug, Default)]
pub struct BridgeStatsRecorder {
    connection_count: Arc<AtomicU64>,
    bytes_sent: Arc<AtomicU64>,
    bytes_received: Arc<AtomicU64>,
    active_connections: ActiveConnectionCounter,
}

impl BridgeStatsRecorder {
    pub fn start_connection(&self) -> ConnectionGuard {
        self.connection_count.fetch_add(1, Ordering::SeqCst);
        self.active_connections.increment();
        ConnectionGuard {
            counter: self.active_connections.clone(),
        }
    }

    fn record_sent(&self, count: usize) {
        self.bytes_sent.fetch_add(count as u64, Ordering::SeqCst);
    }

    fn record_received(&self, count: usize) {
        self.bytes_received
            .fetch_add(count as u64, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> ForwardStats {
        ForwardStats {
            connection_count: self.connection_count.load(Ordering::SeqCst),
            active_connections: self.active_connections.get(),
            bytes_sent: self.bytes_sent.load(Ordering::SeqCst),
            bytes_received: self.bytes_received.load(Ordering::SeqCst),
        }
    }

    pub fn active_connections(&self) -> ActiveConnectionCounter {
        self.active_connections.clone()
    }
}

pub struct ConnectionGuard {
    counter: ActiveConnectionCounter,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.decrement();
    }
}

pub async fn bridge_tcp_to_ssh_stream(
    tcp_stream: TcpStream,
    ssh_stream: BoxedSshForwardStream,
    stats: BridgeStatsRecorder,
    idle_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
    log_prefix: String,
) -> Result<(), ForwardingError> {
    bridge_tcp_to_ssh_stream_inner(
        tcp_stream,
        ssh_stream,
        stats,
        idle_timeout,
        shutdown_rx,
        log_prefix,
        true,
    )
    .await
}

pub(crate) async fn bridge_tcp_to_ssh_stream_with_existing_connection(
    tcp_stream: TcpStream,
    ssh_stream: BoxedSshForwardStream,
    stats: BridgeStatsRecorder,
    idle_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
    log_prefix: String,
) -> Result<(), ForwardingError> {
    bridge_tcp_to_ssh_stream_inner(
        tcp_stream,
        ssh_stream,
        stats,
        idle_timeout,
        shutdown_rx,
        log_prefix,
        false,
    )
    .await
}

async fn bridge_tcp_to_ssh_stream_inner(
    tcp_stream: TcpStream,
    ssh_stream: BoxedSshForwardStream,
    stats: BridgeStatsRecorder,
    idle_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
    log_prefix: String,
    track_connection: bool,
) -> Result<(), ForwardingError> {
    let _connection_guard = track_connection.then(|| stats.start_connection());
    let (tcp_read, tcp_write) = tcp_stream.into_split();
    let (ssh_read, ssh_write) = tokio::io::split(ssh_stream);
    bridge_split_streams_inner(
        tcp_read,
        tcp_write,
        ssh_read,
        ssh_write,
        stats,
        idle_timeout,
        shutdown_rx,
        log_prefix,
    )
    .await
}

async fn bridge_split_streams_inner<LR, LW, SR, SW>(
    tcp_read: LR,
    tcp_write: LW,
    ssh_read: SR,
    ssh_write: SW,
    stats: BridgeStatsRecorder,
    idle_timeout: Duration,
    shutdown_rx: watch::Receiver<bool>,
    log_prefix: String,
) -> Result<(), ForwardingError>
where
    LR: AsyncRead + Send + Unpin,
    LW: AsyncWrite + Send + Unpin,
    SR: AsyncRead + Send + Unpin,
    SW: AsyncWrite + Send + Unpin,
{
    let (activity_tx, activity_rx) = watch::channel(0_u64);

    // Keep both pumps as child futures so ending the bridge cancels every pending read.
    let local_to_ssh = pipe_direction(
        tcp_read,
        ssh_write,
        stats.clone(),
        Direction::LocalToSsh,
        activity_tx.clone(),
    );
    let ssh_to_local = pipe_direction(
        ssh_read,
        tcp_write,
        stats,
        Direction::SshToLocal,
        activity_tx,
    );
    let idle = wait_for_idle(activity_rx, idle_timeout);
    let shutdown = wait_for_shutdown(shutdown_rx);

    tokio::select! {
        result = local_to_ssh => result?,
        result = ssh_to_local => result?,
        idle_elapsed = idle => {
            if idle_elapsed {
                tracing::debug!("{log_prefix}: closing idle forwarding bridge");
            }
        }
        _ = shutdown => {}
    }

    Ok(())
}

pub(crate) async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    if *shutdown_rx.borrow() {
        return;
    }

    loop {
        match shutdown_rx.changed().await {
            Ok(()) if *shutdown_rx.borrow() => return,
            Ok(()) => continue,
            Err(_) => return,
        }
    }
}

async fn wait_for_idle(mut activity_rx: watch::Receiver<u64>, idle_timeout: Duration) -> bool {
    loop {
        match tokio::time::timeout(idle_timeout, activity_rx.changed()).await {
            Ok(Ok(())) => continue,
            Ok(Err(_)) => return false,
            Err(_) => return true,
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    LocalToSsh,
    SshToLocal,
}

async fn pipe_direction<R, W>(
    mut reader: R,
    mut writer: W,
    stats: BridgeStatsRecorder,
    direction: Direction,
    activity_tx: watch::Sender<u64>,
) -> io::Result<()>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin,
{
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<Vec<u8>>(FORWARD_BRIDGE_CHANNEL_CAPACITY);
    let read_activity_tx = activity_tx.clone();
    let reader_pump = async move {
        let mut buffer = vec![0_u8; FORWARD_BRIDGE_READ_BUFFER_SIZE];
        loop {
            let count = reader.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            record_activity(&read_activity_tx);
            if chunk_tx.send(buffer[..count].to_vec()).await.is_err() {
                break;
            }
        }
        Ok::<_, io::Error>(())
    };
    let writer_pump = async move {
        while let Some(chunk) = chunk_rx.recv().await {
            writer.write_all(&chunk).await?;
            record_activity(&activity_tx);
            match direction {
                Direction::LocalToSsh => stats.record_sent(chunk.len()),
                Direction::SshToLocal => stats.record_received(chunk.len()),
            }
        }
        writer.shutdown().await
    };

    // Structured concurrency drops either pump immediately if its sibling fails or is cancelled.
    tokio::try_join!(reader_pump, writer_pump)?;
    Ok(())
}

fn record_activity(activity_tx: &watch::Sender<u64>) {
    // Updating the version lets the idle waiter observe coalesced activity safely.
    activity_tx.send_modify(|version| *version = version.wrapping_add(1));
}
