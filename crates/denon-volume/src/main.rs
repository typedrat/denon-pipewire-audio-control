use std::os::fd::FromRawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use denon_volume::avr::{self, ConnectConfig, Outbox, Target};
use denon_volume::protocol;
use denon_volume::scale::Mv;
use denon_volume::service::ExternalVolume;
use denon_volume::state::Shared;
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// PipeWire device name of the card wired to the receiver.
    #[arg(long)]
    device: String,

    /// Route on that card whose volume the receiver controls.
    #[arg(long, default_value = "hdmi-output-0")]
    route: String,

    /// Receiver input the PC is plugged into, as the receiver names it (e.g. BD).
    #[arg(long)]
    pc_input: String,

    /// Receiver level that 100% maps to; MV 80 is 0 dB.
    #[arg(long, default_value = "70", value_parser = parse_cap)]
    cap_mv: Mv,

    /// Receiver address; skips mDNS discovery.
    #[arg(long)]
    host: Option<String>,

    /// HEOS device ID or friendly name to pick when discovering.
    #[arg(long, conflicts_with = "host")]
    select: Option<String>,

    /// Socket to listen on when not socket-activated.
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn parse_cap(s: &str) -> Result<Mv, String> {
    let level: f64 = s.parse().map_err(|e| format!("{e}"))?;
    let half_steps = level * 2.0;
    if !(0.0..=196.0).contains(&half_steps) || half_steps.fract() != 0.0 {
        return Err("must be between 0 and 98 in steps of 0.5".to_owned());
    }
    Ok(Mv::from_half_steps(half_steps as u16))
}

/// The listener systemd passed in, if this process was socket-activated.
fn activated_listener() -> anyhow::Result<Option<zlink::unix::Listener>> {
    let for_us = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id());
    let fds: u32 = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    if !for_us || fds < 1 {
        return Ok(None);
    }
    // SAFETY: systemd hands over the first socket as fd 3, and nothing else in this process
    // opens or owns it.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
    listener.set_nonblocking(true)?;
    let listener = tokio::net::UnixListener::from_std(listener)?;
    Ok(Some(zlink::unix::Listener::from(listener)))
}

fn default_socket() -> anyhow::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let dir = PathBuf::from(runtime).join("denon-volume");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("org.pipewire.ExternalVolume"))
}

fn bind(path: &Path) -> anyhow::Result<zlink::unix::Listener> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        anyhow::ensure!(
            meta.file_type().is_socket(),
            "{path:?} exists and isn't a socket"
        );
        std::fs::remove_file(path)?;
    }
    zlink::unix::bind(path).with_context(|| format!("binding {path:?}"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();

    let shared = Arc::new(Shared::new(args.pc_input, args.cap_mv));
    let outbox = Arc::new(Outbox::default());
    let config = ConnectConfig {
        target: match args.host {
            Some(host) => Target::Host(host),
            None => Target::Discover {
                selector: args.select,
            },
        },
        port: protocol::PORT,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        heartbeat_interval: Duration::from_secs(30),
    };

    let (listener, bound) = match activated_listener()? {
        Some(listener) => (listener, None),
        None => {
            let path = match args.socket {
                Some(path) => path,
                None => default_socket()?,
            };
            (bind(&path)?, Some(path))
        }
    };
    info!(socket = ?bound, "serving org.pipewire.ExternalVolume");

    let service = ExternalVolume::new(args.device, args.route, shared.clone(), outbox.clone());
    let server = zlink::Server::new(listener, service);

    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;
    tokio::select! {
        result = server.run() => result.context("varlink server failed")?,
        () = avr::run(config, shared, outbox) => {}
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
        _ = sighup.recv() => {}
    }

    if let Some(path) = bound {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_accepts_whole_and_half_steps() {
        assert_eq!(parse_cap("70"), Ok(Mv::from_whole(70)));
        assert_eq!(parse_cap("70.5"), Ok(Mv::from_half_steps(141)));
        assert_eq!(parse_cap("70.500000"), Ok(Mv::from_half_steps(141)));
    }

    #[test]
    fn cap_rejects_off_grid_and_out_of_range() {
        assert!(parse_cap("70.2").is_err());
        assert!(parse_cap("99").is_err());
        assert!(parse_cap("-1").is_err());
        assert!(parse_cap("loud").is_err());
    }
}
