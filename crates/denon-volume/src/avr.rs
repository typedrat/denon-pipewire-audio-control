//! The telnet session with the receiver.

use std::fmt;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::WriteHalf;
use tokio::sync::Notify;
use tokio::time;
use tracing::{debug, info, warn};

use crate::discovery;
use crate::protocol::{self, Command, HEARTBEAT, LineBuffer, QUERIES, TERMINATOR};
use crate::scale::Mv;
use crate::state::Shared;

/// Denon's protocol documentation asks for at least 50 ms between commands.
const COMMAND_SPACING: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Commands waiting to be sent. Only the newest volume and newest mute are kept, so dragging
/// a slider can't build up a backlog for the receiver to work through.
#[derive(Default)]
pub struct Outbox {
    pending: Mutex<Pending>,
    notify: Notify,
}

#[derive(Default)]
struct Pending {
    volume: Option<Mv>,
    mute: Option<bool>,
}

impl Outbox {
    pub fn push(&self, command: Command) {
        let mut pending = self.pending.lock().expect("outbox lock poisoned");
        match command {
            Command::Volume(mv) => pending.volume = Some(mv),
            Command::Mute(mute) => pending.mute = Some(mute),
        }
        drop(pending);
        self.notify.notify_one();
    }

    pub fn take(&self) -> Option<Command> {
        let mut pending = self.pending.lock().expect("outbox lock poisoned");
        // A pending mute goes first: it is the change a listener notices soonest.
        if let Some(mute) = pending.mute.take() {
            return Some(Command::Mute(mute));
        }
        pending.volume.take().map(Command::Volume)
    }

    pub async fn next(&self) -> Command {
        loop {
            if let Some(command) = self.take() {
                return command;
            }
            self.notify.notified().await;
        }
    }

    /// Drops commands queued while disconnected; they were decided against state that may
    /// no longer hold.
    pub fn clear(&self) {
        *self.pending.lock().expect("outbox lock poisoned") = Pending::default();
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    Host(String),
    Discover { selector: Option<String> },
}

#[derive(Debug, Clone)]
pub struct ConnectConfig {
    pub target: Target,
    pub port: u16,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub heartbeat_interval: Duration,
}

/// Keeps a session with the receiver open forever, reconnecting with exponential backoff.
pub async fn run(config: ConnectConfig, shared: Arc<Shared>, outbox: Arc<Outbox>) {
    let mut backoff = config.initial_backoff;
    loop {
        match connect(&config).await {
            Ok(stream) => {
                info!(peer = ?stream.peer_addr().ok(), "connected to receiver");
                backoff = config.initial_backoff;
                outbox.clear();
                shared.set_connected(true);
                if let Err(e) = session(stream, &shared, &outbox, config.heartbeat_interval).await {
                    warn!("receiver session ended: {e:#}");
                }
                shared.set_connected(false);
            }
            Err(e) => warn!("could not reach receiver: {e:#}"),
        }
        time::sleep(backoff).await;
        backoff = (backoff * 2).min(config.max_backoff);
    }
}

async fn connect(config: &ConnectConfig) -> anyhow::Result<TcpStream> {
    let host = match &config.target {
        Target::Host(host) => host.clone(),
        Target::Discover { selector } => discovery::find(selector.as_deref(), DISCOVERY_TIMEOUT)
            .await?
            .to_string(),
    };
    let stream = time::timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((host.as_str(), config.port)),
    )
    .await
    .with_context(|| format!("timed out connecting to {host}"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

async fn session(
    mut stream: TcpStream,
    shared: &Shared,
    outbox: &Outbox,
    heartbeat_interval: Duration,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = stream.split();
    for query in QUERIES {
        send(&mut writer, query).await?;
        time::sleep(COMMAND_SPACING).await;
    }

    let mut lines = LineBuffer::default();
    let mut buf = [0u8; 1024];
    let mut heartbeat = time::interval(heartbeat_interval);
    heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    // A tokio interval fires immediately; consume that so the first heartbeat is one full
    // interval after the queries.
    heartbeat.tick().await;

    // The receiver answers every heartbeat, so a connection that stays silent for two
    // intervals has lost its peer. Without this, a peer that vanished without a FIN or RST
    // leaves the read pending while writes keep succeeding into the kernel send buffer.
    let idle_timeout = heartbeat_interval * 2;
    let mut last_received = time::Instant::now();

    loop {
        tokio::select! {
            read = reader.read(&mut buf) => {
                let n = read?;
                anyhow::ensure!(n > 0, "receiver closed the connection");
                last_received = time::Instant::now();
                for line in lines.push(&buf[..n]) {
                    debug!(line, "from receiver");
                    if let Some(event) = protocol::parse_line(&line) {
                        shared.apply(event);
                    }
                }
            }
            command = outbox.next() => {
                debug!(?command, "to receiver");
                send(&mut writer, command).await?;
                time::sleep(COMMAND_SPACING).await;
            }
            _ = heartbeat.tick() => {
                send(&mut writer, HEARTBEAT).await?;
                time::sleep(COMMAND_SPACING).await;
            }
            () = time::sleep_until(last_received + idle_timeout) => {
                anyhow::bail!("receiver stopped answering");
            }
        }
    }
}

async fn send(writer: &mut WriteHalf<'_>, line: impl fmt::Display) -> io::Result<()> {
    writer
        .write_all(format!("{line}{TERMINATOR}").as_bytes())
        .await
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::state::AvrState;

    fn fixtures() -> (Arc<Shared>, Arc<Outbox>) {
        (
            Arc::new(Shared::new("BD", Mv::from_whole(70))),
            Arc::new(Outbox::default()),
        )
    }

    async fn listen() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    fn config(port: u16) -> ConnectConfig {
        ConnectConfig {
            target: Target::Host("127.0.0.1".into()),
            port,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
            // Long enough that the idle deadline never fires in tests whose fake receiver
            // stays quiet; the dead-peer test shortens it.
            heartbeat_interval: Duration::from_secs(60),
        }
    }

    async fn with_daemon(
        config: ConnectConfig,
        shared: Arc<Shared>,
        outbox: Arc<Outbox>,
        body: impl Future<Output = ()>,
    ) {
        tokio::select! {
            () = run(config, shared, outbox) => unreachable!("run returned"),
            () = body => {}
        }
    }

    async fn read_until(sock: &mut TcpStream, needle: &str) -> String {
        let mut got = String::new();
        let mut buf = [0u8; 256];
        time::timeout(Duration::from_secs(2), async {
            while !got.contains(needle) {
                let n = sock.read(&mut buf).await.unwrap();
                assert!(n > 0, "daemon closed the connection");
                got.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {needle:?}"));
        got
    }

    async fn wait_for(shared: &Shared, pred: impl Fn(&AvrState) -> bool) {
        let mut rx = shared.subscribe();
        time::timeout(Duration::from_secs(2), rx.wait_for(|s| pred(s)))
            .await
            .expect("timed out waiting for state")
            .expect("state sender dropped");
    }

    #[test]
    fn outbox_keeps_only_the_newest_of_each_kind() {
        let outbox = Outbox::default();
        outbox.push(Command::Volume(Mv::from_whole(50)));
        outbox.push(Command::Volume(Mv::from_whole(55)));
        outbox.push(Command::Mute(true));
        outbox.push(Command::Mute(false));
        assert_eq!(outbox.take(), Some(Command::Mute(false)));
        assert_eq!(outbox.take(), Some(Command::Volume(Mv::from_whole(55))));
        assert_eq!(outbox.take(), None);
    }

    #[tokio::test]
    async fn seeds_state_from_queries() {
        let (listener, port) = listen().await;
        let (shared, outbox) = fixtures();
        with_daemon(config(port), shared.clone(), outbox, async {
            let (mut sock, _) = listener.accept().await.unwrap();
            let sent = read_until(&mut sock, "MU?\r").await;
            for query in QUERIES {
                let line = format!("{query}\r");
                assert!(sent.contains(&line), "missing {line:?} in {sent:?}");
            }
            sock.write_all(b"ZMON\rSIBD\rMV62\rMVMAX 98\rMUOFF\r")
                .await
                .unwrap();
            wait_for(&shared, |s| s.mute.is_some()).await;
            let s = shared.snapshot();
            assert!(s.connected);
            assert_eq!(s.power, Some(true));
            assert_eq!(s.input.as_deref(), Some("BD"));
            assert_eq!(s.mv, Some(Mv::from_whole(62)));
            assert_eq!(s.mute, Some(false));
        })
        .await;
    }

    #[tokio::test]
    async fn sends_queued_commands() {
        let (listener, port) = listen().await;
        let (shared, outbox) = fixtures();
        with_daemon(config(port), shared, outbox.clone(), async {
            let (mut sock, _) = listener.accept().await.unwrap();
            read_until(&mut sock, "MU?\r").await;
            outbox.push(Command::Volume(Mv::from_half_steps(121)));
            outbox.push(Command::Mute(true));
            let sent = read_until(&mut sock, "MV605\r").await;
            assert!(sent.contains("MUON\r"));
        })
        .await;
    }

    #[tokio::test]
    async fn reconnects_after_the_receiver_drops() {
        let (listener, port) = listen().await;
        let (shared, outbox) = fixtures();
        with_daemon(config(port), shared.clone(), outbox, async {
            let (sock, _) = listener.accept().await.unwrap();
            wait_for(&shared, |s| s.connected).await;
            drop(sock);
            wait_for(&shared, |s| !s.connected).await;
            let (_sock, _) = time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .expect("no reconnect")
                .unwrap();
            wait_for(&shared, |s| s.connected).await;
        })
        .await;
    }

    #[tokio::test]
    async fn drops_a_receiver_that_stops_answering() {
        let (listener, port) = listen().await;
        let (shared, outbox) = fixtures();
        let config = ConnectConfig {
            heartbeat_interval: Duration::from_millis(100),
            ..config(port)
        };
        with_daemon(config, shared.clone(), outbox, async {
            let (_sock, _) = listener.accept().await.unwrap();
            wait_for(&shared, |s| s.connected).await;
            wait_for(&shared, |s| !s.connected).await;
            let (_sock, _) = time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .expect("no reconnect")
                .unwrap();
        })
        .await;
    }
}
