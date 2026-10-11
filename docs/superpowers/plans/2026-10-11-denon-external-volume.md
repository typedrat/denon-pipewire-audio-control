# Denon External Volume Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make PipeWire's volume slider for the PC's HDMI output drive a Denon AVR-S740H's main-zone volume and mute, with live sync back from the receiver.

**Architecture:** `denon-volume` is a Rust daemon that serves `org.pipewire.ExternalVolume` over Varlink (socket-activated, systemd user unit) and keeps one telnet session to the receiver, found by mDNS. PipeWire's ALSA ACP code (Valve's out-of-tree patches 0001–0018) calls the daemon. Our patches 0019 (bug fixes) and 0020 (Monitor support) make reads, rapid writes and live updates work. A NixOS module applies the patches to the system PipeWire and WirePlumber and runs the daemon.

**Tech Stack:** Rust 2024 (tokio, zlink 0.4, mdns-sd 0.21, clap 4, tracing), C (PipeWire 1.6.x SPA/ACP), Nix (flake-parts, crane, numtide/devshell, treefmt-nix).

**Spec:** `docs/superpowers/specs/2026-10-11-denon-external-volume-design.md`

## Global Constraints

- Rust edition `2024`; the virtual workspace manifest spells out `resolver = "3"`.
- Flake built with flake-parts; Rust built with crane; dev shell from numtide/devshell; formatting by treefmt-nix (nixfmt + rustfmt).
- Varlink interface `org.pipewire.ExternalVolume`, exactly as in Valve's IDL (`GetCapabilities`, `ReadVolume`, `ReadMute`, `Monitor`, `WriteVolumeAbsolute`, `WriteVolumeRelative`, `WriteMuteValue`, `WriteMuteToggle`).
- Volume mapping: `dB = cap_dB + 60·log10(v)`, `MV = dB + 80`, rounded to 0.5 dB, clamped to `[MV 0, cap]`; `v ≤ 0` → MV 0. Default cap **MV 70** (−10 dB).
- Writes reach the receiver only while it is connected, powered on (`ZMON`) and on the configured PC input; otherwise they are dropped and the real state is re-published.
- Default device/route: `alsa_card.pci-0000_01_00.1` / `hdmi-output-0` (measured on this machine). PC input is `BD` here, but it is a required option with no default.
- Socket path: `$XDG_RUNTIME_DIR/denon-volume/org.pipewire.ExternalVolume`.
- Comments explain *why*, are written for a cold reader, never reference other files or the change history, and match the surrounding density (user's global CLAUDE.md).
- Commit messages carry no attribution trailers.
- Never run `find /` or recursive greps from `/`.

## Spec deviations (decided during planning)

These came from reading the actual PipeWire/zlink code. Task 11 updates the spec to match.

1. **Telnet parsing gets its own pure module** (`protocol.rs`), separate from the socket handling in `avr.rs`.
2. **Power is tracked with `ZM` (main zone), not `PW`.** `ZM?` / `ZMON` / `ZMOFF` are tracked, and `PWSTANDBY` also counts as off.
3. **Two PipeWire patches instead of one.** 0019 fixes bugs in Valve's series that block our use:
   - stale volume written;
   - `ReadVolume`/`ReadMute` parse the wrong JSON token;
   - the varlink client drops `continues`;
   - async writes fail while a reply is pending, so ACP writes go out `oneway`;
   - per-device re-init leaks clients.

   0020 adds Monitor support with reconnect.
4. **Startup:** `GetCapabilities` waits up to 1.5 s for the first receiver state (ACP allows 2 s), `ReadVolume`/`ReadMute` return `OperationUnsupported` until the state is known, and Monitor only emits once the level is known.
5. **Echo suppression:** the daemon remembers the last requested fraction and its rounded level. While the receiver reports that level, it reports the exact fraction back, so PipeWire never sees half-dB rounding fight a slider drag. A write does not notify Monitor until the receiver confirms it.
6. **No multiple-receiver warning in discovery:** the first matching answer wins.
7. **Service tests drive the daemon with a `zlink` proxy in-process** instead of `varlinkctl`, which stays a manual check. Command spacing is checked by code review, because a timing test over real TCP would be flaky.

## File structure

```
.gitignore
Cargo.toml                          # virtual workspace
Cargo.lock
flake.nix                           # flake-parts: packages, checks, devshell, treefmt, nixosModules, lib
nix/patched.nix                     # patched pipewire + wireplumber builders
nix/module.nix                      # services.denon-volume
patches/README.md                   # provenance of the vendored patches
patches/pipewire/0001…0018-*.patch  # Valve/Collabora, vendored verbatim
patches/pipewire/0019-*.patch       # ours: ext-volume fixes
patches/pipewire/0020-*.patch       # ours: Monitor support
patches/wireplumber/0001-*.patch    # Collabora, vendored verbatim
crates/denon-volume/Cargo.toml
crates/denon-volume/src/lib.rs      # module list
crates/denon-volume/src/main.rs     # CLI, socket activation, signals
crates/denon-volume/src/scale.rs    # Mv type, fraction ↔ MV mapping
crates/denon-volume/src/protocol.rs # telnet line parsing/formatting
crates/denon-volume/src/state.rs    # AvrState, Shared (watch channel + write policy)
crates/denon-volume/src/avr.rs      # Outbox, telnet session, reconnect loop
crates/denon-volume/src/discovery.rs# mDNS lookup
crates/denon-volume/src/types.rs    # Varlink wire types
crates/denon-volume/src/service.rs  # ExternalVolume service
crates/denon-volume/tests/service.rs# end-to-end Varlink tests
```

---

### Task 1: Workspace and flake scaffold

**Files:**
- Create: `.gitignore`, `Cargo.toml`, `crates/denon-volume/Cargo.toml`, `crates/denon-volume/src/lib.rs`, `crates/denon-volume/src/main.rs`, `flake.nix`

**Interfaces:**
- Produces: crate `denon-volume` (lib `denon_volume` + bin `denon-volume`); flake outputs `packages.<system>.{default,denon-volume}`, `checks.<system>.{denon-volume,clippy,test,treefmt}`, `devShells.<system>.default`, `formatter`.

- [ ] **Step 1: Write `.gitignore`**

```gitignore
/target
/result*
/.work
/.direnv
```

- [ ] **Step 2: Write the workspace manifest `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = ["crates/denon-volume"]
```

- [ ] **Step 3: Write `crates/denon-volume/Cargo.toml`**

```toml
[package]
name = "denon-volume"
version = "0.1.0"
edition = "2024"
license = "MIT"
description = "Drives a Denon AVR's volume from PipeWire via org.pipewire.ExternalVolume"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
futures-util = "0.3"
mdns-sd = "0.21"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt", "macros", "net", "io-util", "time", "sync", "signal"] }
tokio-stream = { version = "0.1", features = ["sync"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
zlink = "0.4"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Write `crates/denon-volume/src/lib.rs` and a placeholder `main.rs`**

`lib.rs`:
```rust
//! Drives a Denon AVR's master volume from PipeWire through `org.pipewire.ExternalVolume`.
```

`main.rs` (Task 8 replaces it):
```rust
fn main() {}
```

- [ ] **Step 5: Build and generate `Cargo.lock`**

Run: `cargo build`
Expected: `Finished` with no errors; `Cargo.lock` exists.

- [ ] **Step 6: Write `flake.nix`**

```nix
{
  description = "Denon AVR volume control for PipeWire via org.pipewire.ExternalVolume";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    crane.url = "github:ipetkov/crane";
    devshell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      imports = [
        inputs.devshell.flakeModule
        inputs.treefmt-nix.flakeModule
      ];

      perSystem =
        { config, pkgs, ... }:
        let
          craneLib = inputs.crane.mkLib pkgs;
          commonArgs = {
            src = craneLib.cleanCargoSource ./.;
            strictDeps = true;
            pname = "denon-volume";
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          denon-volume = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              doCheck = false;
            }
          );
        in
        {
          packages = {
            default = denon-volume;
            inherit denon-volume;
          };

          checks = {
            inherit denon-volume;
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- --deny warnings";
              }
            );
            test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });
          };

          treefmt = {
            projectRootFile = "flake.nix";
            programs.nixfmt.enable = true;
            programs.rustfmt.enable = true;
          };

          devshells.default = {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              gcc
              config.treefmt.build.wrapper
            ];
            env = [
              {
                name = "RUST_SRC_PATH";
                value = "${pkgs.rustPlatform.rustLibSrc}";
              }
            ];
          };
        };
    };
}
```

- [ ] **Step 7: Stage files (flakes only see tracked files) and lock the flake**

Run: `git add -A && nix flake lock`
Expected: `flake.lock` created.

- [ ] **Step 8: Run the checks**

Run: `git add flake.lock && nix flake check -L`
Expected: all checks build: `denon-volume`, `clippy`, `test` (0 tests) and `treefmt`. If `treefmt` fails on formatting, run `nix fmt`, re-stage and re-run.

- [ ] **Step 9: Verify the dev shell**

Run: `nix develop -c cargo --version`
Expected: prints a cargo version.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "Scaffold denon-volume crate and flake"
```

---

### Task 2: `scale` — the MV type and the volume mapping

**Files:**
- Create: `crates/denon-volume/src/scale.rs`
- Modify: `crates/denon-volume/src/lib.rs` (add `pub mod scale;`)

**Interfaces:**
- Produces:
  - `pub struct Mv` (Copy, Eq, Ord, Hash, Debug)
  - `Mv::ZERO`, `Mv::from_half_steps(u16) -> Mv`, `Mv::from_whole(u16) -> Mv`, `Mv::half_steps(self) -> u16`, `Mv::db(self) -> f64`, `Mv::parse(&str) -> Option<Mv>`
  - `impl Display for Mv` (wire digits: `62`, `605`, `00`, `005`)
  - `pub fn to_mv(v: f64, cap: Mv) -> Mv`
  - `pub fn from_mv(mv: Mv, cap: Mv) -> f64`

- [ ] **Step 1: Write the failing tests** in `crates/denon-volume/src/scale.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Mv = Mv::from_whole(70);

    #[test]
    fn parses_wire_forms() {
        assert_eq!(Mv::parse("62"), Some(Mv::from_half_steps(124)));
        assert_eq!(Mv::parse("605"), Some(Mv::from_half_steps(121)));
        assert_eq!(Mv::parse("00"), Some(Mv::ZERO));
        assert_eq!(Mv::parse("005"), Some(Mv::from_half_steps(1)));
        assert_eq!(Mv::parse("6"), None);
        assert_eq!(Mv::parse("604"), None);
        assert_eq!(Mv::parse("MAX 98"), None);
    }

    #[test]
    fn formats_wire_forms() {
        assert_eq!(Mv::from_half_steps(124).to_string(), "62");
        assert_eq!(Mv::from_half_steps(121).to_string(), "605");
        assert_eq!(Mv::ZERO.to_string(), "00");
        assert_eq!(Mv::from_half_steps(1).to_string(), "005");
    }

    #[test]
    fn current_level_is_74_percent() {
        assert_eq!(to_mv(0.736, CAP), Mv::from_whole(62));
        assert!((from_mv(Mv::from_whole(62), CAP) - 0.7356).abs() < 1e-3);
    }

    #[test]
    fn full_scale_is_the_cap() {
        assert_eq!(to_mv(1.0, CAP), CAP);
        assert_eq!(to_mv(1.5, CAP), CAP);
    }

    #[test]
    fn silence_and_garbage_map_to_zero() {
        assert_eq!(to_mv(0.0, CAP), Mv::ZERO);
        assert_eq!(to_mv(-1.0, CAP), Mv::ZERO);
        assert_eq!(to_mv(f64::NAN, CAP), Mv::ZERO);
        assert_eq!(to_mv(1e-6, CAP), Mv::ZERO);
        assert_eq!(from_mv(Mv::ZERO, CAP), 0.0);
    }

    #[test]
    fn every_step_round_trips() {
        for half_steps in 1..=CAP.half_steps() {
            let mv = Mv::from_half_steps(half_steps);
            assert_eq!(to_mv(from_mv(mv, CAP), CAP), mv, "{mv}");
        }
    }

    #[test]
    fn levels_above_the_cap_read_above_one() {
        assert!(from_mv(Mv::from_whole(80), CAP) > 1.0);
    }
}
```

Add `pub mod scale;` to `lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p denon-volume scale`
Expected: compile errors (`Mv`, `to_mv` and `from_mv` are not defined).

- [ ] **Step 3: Implement** (above the test module in `scale.rs`)

```rust
//! Conversion between PipeWire's volume fraction and the receiver's master volume.
//!
//! PipeWire volumes are cubic: a fraction `v` is an amplitude of `v³`, which is
//! `20·log10(v³) = 60·log10(v)` dB. The receiver's `MV` scale is dB offset by 80 (MV 80 is
//! 0 dB) in half-dB steps. `cap` is the level that 100% maps to.

use std::fmt;

/// A master-volume level counted in half steps: `MV62` is 124 and `MV605` is 121.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mv(u16);

impl Mv {
    pub const ZERO: Mv = Mv(0);

    pub const fn from_half_steps(half_steps: u16) -> Self {
        Mv(half_steps)
    }

    pub const fn from_whole(whole: u16) -> Self {
        Mv(whole * 2)
    }

    pub const fn half_steps(self) -> u16 {
        self.0
    }

    pub fn db(self) -> f64 {
        f64::from(self.0) / 2.0 - 80.0
    }

    /// Parses the digits after `MV`: two digits, optionally followed by `5` for a half step.
    pub fn parse(digits: &str) -> Option<Self> {
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        match digits.len() {
            2 => digits.parse().ok().map(Mv::from_whole),
            3 if digits.ends_with('5') => digits[..2].parse::<u16>().ok().map(|w| Mv(w * 2 + 1)),
            _ => None,
        }
    }
}

impl fmt::Display for Mv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}", self.0 / 2)?;
        if self.0 % 2 == 1 {
            f.write_str("5")?;
        }
        Ok(())
    }
}

/// Maps a PipeWire fraction to the nearest half step, never above `cap`.
pub fn to_mv(v: f64, cap: Mv) -> Mv {
    if v.is_nan() || v <= 0.0 {
        return Mv::ZERO;
    }
    let db = cap.db() + 60.0 * v.log10();
    let half_steps = ((db + 80.0) * 2.0).round().clamp(0.0, f64::from(cap.0));
    Mv(half_steps as u16)
}

/// Maps a receiver level back to a PipeWire fraction. Levels above `cap`, set from the
/// receiver itself, come out above 1.0.
pub fn from_mv(mv: Mv, cap: Mv) -> f64 {
    if mv == Mv::ZERO {
        0.0
    } else {
        10f64.powf((mv.db() - cap.db()) / 60.0)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p denon-volume scale`
Expected: 7 passed.

- [ ] **Step 5: Lint**

Run: `cargo clippy --all-targets -- --deny warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/denon-volume/src/scale.rs crates/denon-volume/src/lib.rs
git commit -m "Add MV type and PipeWire volume mapping"
```

---

### Task 3: `protocol` — telnet lines

**Files:**
- Create: `crates/denon-volume/src/protocol.rs`
- Modify: `crates/denon-volume/src/lib.rs` (add `pub mod protocol;`)

**Interfaces:**
- Consumes: `scale::Mv` (`Mv::parse`, `Display`).
- Produces:
  - `pub const PORT: u16 = 23`
  - `pub const QUERIES: [&str; 4]`, each ending in `\r`
  - `pub const HEARTBEAT: &str`
  - `pub enum Event { Power(bool), Input(String), Volume(Mv), Mute(bool) }` (Debug, Clone, PartialEq, Eq)
  - `pub fn parse_line(&str) -> Option<Event>`
  - `pub enum Command { Volume(Mv), Mute(bool) }` (Debug, Clone, Copy, PartialEq, Eq) with `fn wire(self) -> String`
  - `pub struct LineBuffer` (Default) with `fn push(&mut self, &[u8]) -> Vec<String>`

- [ ] **Step 1: Write the failing tests** in `protocol.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tracked_events() {
        assert_eq!(parse_line("ZMON"), Some(Event::Power(true)));
        assert_eq!(parse_line("ZMOFF"), Some(Event::Power(false)));
        assert_eq!(parse_line("PWSTANDBY"), Some(Event::Power(false)));
        assert_eq!(parse_line("SIBD"), Some(Event::Input("BD".into())));
        assert_eq!(parse_line("SISAT/CBL"), Some(Event::Input("SAT/CBL".into())));
        assert_eq!(parse_line("MV62"), Some(Event::Volume(Mv::from_whole(62))));
        assert_eq!(parse_line("MV605"), Some(Event::Volume(Mv::from_half_steps(121))));
        assert_eq!(parse_line("MUON"), Some(Event::Mute(true)));
        assert_eq!(parse_line("MUOFF"), Some(Event::Mute(false)));
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(parse_line("MVMAX 98"), None);
        assert_eq!(parse_line("PWON"), None);
        assert_eq!(parse_line("SVOFF"), None);
        assert_eq!(parse_line("SI"), None);
        assert_eq!(parse_line("Z2MU OFF"), None);
    }

    #[test]
    fn formats_commands() {
        assert_eq!(Command::Volume(Mv::from_whole(62)).wire(), "MV62\r");
        assert_eq!(Command::Volume(Mv::from_half_steps(121)).wire(), "MV605\r");
        assert_eq!(Command::Mute(true).wire(), "MUON\r");
        assert_eq!(Command::Mute(false).wire(), "MUOFF\r");
    }

    #[test]
    fn reassembles_split_lines() {
        let mut lines = LineBuffer::default();
        assert_eq!(lines.push(b"MV6"), Vec::<String>::new());
        assert_eq!(lines.push(b"2\rMU"), vec!["MV62"]);
        assert_eq!(lines.push(b"OFF\r\nSIBD\r"), vec!["MUOFF", "SIBD"]);
    }
}
```

Add `pub mod protocol;` to `lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p denon-volume protocol`
Expected: compile errors (the items are not defined).

- [ ] **Step 3: Implement** (above the tests)

```rust
//! The Denon AVR telnet control protocol: `\r`-terminated ASCII lines on TCP port 23.

use crate::scale::Mv;

pub const PORT: u16 = 23;

/// Sent after connecting so the state is known without waiting for something to change.
pub const QUERIES: [&str; 4] = ["ZM?\r", "SI?\r", "MV?\r", "MU?\r"];

/// Sent periodically: a TCP write is the only way to notice a receiver that dropped off the
/// network without closing the connection.
pub const HEARTBEAT: &str = "ZM?\r";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Power(bool),
    Input(String),
    Volume(Mv),
    Mute(bool),
}

/// Parses one line from the receiver; lines this daemon doesn't track yield `None`.
pub fn parse_line(line: &str) -> Option<Event> {
    match line {
        "ZMON" => return Some(Event::Power(true)),
        "ZMOFF" | "PWSTANDBY" => return Some(Event::Power(false)),
        "MUON" => return Some(Event::Mute(true)),
        "MUOFF" => return Some(Event::Mute(false)),
        _ => {}
    }
    if let Some(digits) = line.strip_prefix("MV") {
        return Mv::parse(digits).map(Event::Volume);
    }
    if let Some(input) = line.strip_prefix("SI")
        && !input.is_empty()
    {
        return Some(Event::Input(input.to_owned()));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Volume(Mv),
    Mute(bool),
}

impl Command {
    pub fn wire(self) -> String {
        match self {
            Command::Volume(mv) => format!("MV{mv}\r"),
            Command::Mute(true) => "MUON\r".to_owned(),
            Command::Mute(false) => "MUOFF\r".to_owned(),
        }
    }
}

/// Reassembles lines from TCP reads, which can split or merge them arbitrarily.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub fn push(&mut self, data: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(data);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == b'\r') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..end]).trim().to_owned();
            if !text.is_empty() {
                lines.push(text);
            }
        }
        lines
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p denon-volume protocol`
Expected: 4 passed.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets -- --deny warnings` (expected: clean)

```bash
git add crates/denon-volume/src/protocol.rs crates/denon-volume/src/lib.rs
git commit -m "Add Denon telnet protocol parsing"
```

---

### Task 4: `state` — shared receiver state and write policy

**Files:**
- Create: `crates/denon-volume/src/state.rs`
- Modify: `crates/denon-volume/src/lib.rs` (add `pub mod state;`)

**Interfaces:**
- Consumes: `protocol::Event`, `scale::{Mv, to_mv, from_mv}`.
- Produces:
  - `pub struct AvrState { pub connected: bool, pub power: Option<bool>, pub input: Option<String>, pub mv: Option<Mv>, pub mute: Option<bool>, /* private: requested, generation */ }` (Debug, Clone, Default, PartialEq)
  - `AvrState::volume(&self, cap: Mv) -> Option<f64>`, `AvrState::active(&self, pc_input: &str) -> bool`
  - `pub struct Shared` with:
    - `new(pc_input: impl Into<String>, cap: Mv) -> Shared`
    - `cap(&self) -> Mv`, `subscribe(&self) -> watch::Receiver<AvrState>`, `snapshot(&self) -> AvrState`
    - `set_connected(&self, bool)`, `apply(&self, Event)`
    - `request_volume(&self, f64) -> Option<Mv>`, `request_mute(&self, bool) -> bool`

- [ ] **Step 1: Write the failing tests** in `state.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Mv = Mv::from_whole(70);

    fn on_pc_input() -> Shared {
        let shared = Shared::new("BD", CAP);
        shared.set_connected(true);
        shared.apply(Event::Power(true));
        shared.apply(Event::Input("BD".into()));
        shared.apply(Event::Volume(Mv::from_whole(62)));
        shared.apply(Event::Mute(false));
        shared
    }

    #[test]
    fn unknown_level_has_no_volume() {
        assert_eq!(Shared::new("BD", CAP).snapshot().volume(CAP), None);
    }

    #[test]
    fn active_needs_connection_power_and_input() {
        let shared = on_pc_input();
        assert!(shared.snapshot().active("BD"));
        shared.apply(Event::Input("GAME".into()));
        assert!(!shared.snapshot().active("BD"));
        shared.apply(Event::Input("BD".into()));
        shared.apply(Event::Power(false));
        assert!(!shared.snapshot().active("BD"));
        shared.apply(Event::Power(true));
        shared.set_connected(false);
        assert!(!shared.snapshot().active("BD"));
    }

    #[test]
    fn confirmed_request_reports_the_exact_fraction() {
        let shared = on_pc_input();
        let mv = shared.request_volume(0.9).expect("active");
        assert_eq!(mv, Mv::from_half_steps(135));
        shared.apply(Event::Volume(mv));
        assert_eq!(shared.snapshot().volume(CAP), Some(0.9));
        shared.apply(Event::Volume(Mv::from_whole(60)));
        assert_eq!(shared.snapshot().volume(CAP), Some(scale::from_mv(Mv::from_whole(60), CAP)));
    }

    #[test]
    fn requests_above_full_scale_are_clamped() {
        let shared = on_pc_input();
        let mv = shared.request_volume(1.4).expect("active");
        assert_eq!(mv, CAP);
        shared.apply(Event::Volume(mv));
        assert_eq!(shared.snapshot().volume(CAP), Some(1.0));
    }

    #[test]
    fn request_waits_for_confirmation_before_notifying() {
        let shared = on_pc_input();
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        let mv = shared.request_volume(0.9).expect("active");
        assert!(!rx.has_changed().unwrap());
        shared.apply(Event::Volume(mv));
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn inactive_requests_are_refused_and_republished() {
        let shared = on_pc_input();
        shared.apply(Event::Input("GAME".into()));
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        assert_eq!(shared.request_volume(0.5), None);
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        assert!(!shared.request_mute(true));
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn repeated_events_do_not_notify() {
        let shared = on_pc_input();
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        shared.apply(Event::Volume(Mv::from_whole(62)));
        shared.apply(Event::Power(true));
        shared.apply(Event::Input("BD".into()));
        shared.set_connected(true);
        assert!(!rx.has_changed().unwrap());
    }
}
```

Add `pub mod state;` to `lib.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p denon-volume state`
Expected: compile errors.

- [ ] **Step 3: Implement** (above the tests)

```rust
//! The daemon's view of the receiver, shared between the telnet session and Varlink clients.

use std::mem;

use tokio::sync::watch;

use crate::protocol::Event;
use crate::scale::{self, Mv};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AvrState {
    pub connected: bool,
    pub power: Option<bool>,
    pub input: Option<String>,
    pub mv: Option<Mv>,
    pub mute: Option<bool>,
    /// The last fraction PipeWire asked for and the level it rounded to. While the receiver
    /// sits at that level the exact fraction is reported, so PipeWire sees its own write
    /// echoed unchanged rather than nudged by half-dB rounding.
    requested: Option<(Mv, f64)>,
    /// Bumped to wake subscribers without changing anything they can see.
    generation: u64,
}

impl AvrState {
    pub fn volume(&self, cap: Mv) -> Option<f64> {
        let mv = self.mv?;
        Some(match self.requested {
            Some((requested, v)) if requested == mv => v,
            _ => scale::from_mv(mv, cap),
        })
    }

    /// Whether writes from PipeWire should reach the receiver.
    pub fn active(&self, pc_input: &str) -> bool {
        self.connected && self.power == Some(true) && self.input.as_deref() == Some(pc_input)
    }
}

/// Holds the receiver state and decides which PipeWire requests become receiver commands.
pub struct Shared {
    tx: watch::Sender<AvrState>,
    pc_input: String,
    cap: Mv,
}

impl Shared {
    pub fn new(pc_input: impl Into<String>, cap: Mv) -> Self {
        Self {
            tx: watch::Sender::new(AvrState::default()),
            pc_input: pc_input.into(),
            cap,
        }
    }

    pub fn cap(&self) -> Mv {
        self.cap
    }

    pub fn subscribe(&self) -> watch::Receiver<AvrState> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> AvrState {
        self.tx.borrow().clone()
    }

    pub fn set_connected(&self, connected: bool) {
        self.tx
            .send_if_modified(|s| mem::replace(&mut s.connected, connected) != connected);
    }

    pub fn apply(&self, event: Event) {
        self.tx.send_if_modified(|s| match event {
            Event::Power(on) => mem::replace(&mut s.power, Some(on)) != Some(on),
            Event::Input(input) => {
                let changed = s.input.as_deref() != Some(input.as_str());
                s.input = Some(input);
                changed
            }
            Event::Volume(mv) => mem::replace(&mut s.mv, Some(mv)) != Some(mv),
            Event::Mute(mute) => mem::replace(&mut s.mute, Some(mute)) != Some(mute),
        });
    }

    /// Returns the level to send. `None` means the receiver isn't listening to the PC, and
    /// subscribers are woken so PipeWire snaps back to the receiver's real level.
    pub fn request_volume(&self, v: f64) -> Option<Mv> {
        let mut target = None;
        self.tx.send_if_modified(|s| {
            if !s.active(&self.pc_input) {
                s.generation += 1;
                return true;
            }
            let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
            let mv = scale::to_mv(v, self.cap);
            s.requested = Some((mv, v));
            target = Some(mv);
            // Subscribers hear about the new level once the receiver confirms it; notifying
            // now would report the old level and drag PipeWire's slider back mid-change.
            false
        });
        target
    }

    /// Returns whether to send the mute change, with the same snap-back as `request_volume`.
    pub fn request_mute(&self, _mute: bool) -> bool {
        let mut active = false;
        self.tx.send_if_modified(|s| {
            active = s.active(&self.pc_input);
            if !active {
                s.generation += 1;
            }
            !active
        });
        active
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p denon-volume state`
Expected: 7 passed.

- [ ] **Step 5: Lint and commit**

Run: `cargo clippy --all-targets -- --deny warnings` (expected: clean)

```bash
git add crates/denon-volume/src/state.rs crates/denon-volume/src/lib.rs
git commit -m "Add shared receiver state with input gating"
```

---

### Task 5: `avr` — outbox and telnet session

**Files:**
- Create: `crates/denon-volume/src/avr.rs`, `crates/denon-volume/src/discovery.rs` (stub; Task 6 fills it in)
- Modify: `crates/denon-volume/src/lib.rs` (add `pub mod avr;` and `pub mod discovery;`)

**Interfaces:**
- Consumes: `protocol::{Command, LineBuffer, parse_line, QUERIES, HEARTBEAT}`, `state::Shared`, `discovery::find(Option<&str>, Duration) -> anyhow::Result<Ipv4Addr>`.
- Produces:
  - `pub struct Outbox` (Default) with `push(&self, Command)`, `take(&self) -> Option<Command>`, `async next(&self) -> Command`, `clear(&self)`
  - `pub enum Target { Host(String), Discover { selector: Option<String> } }`
  - `pub struct ConnectConfig { pub target: Target, pub port: u16, pub initial_backoff: Duration, pub max_backoff: Duration }`
  - `pub async fn run(config: ConnectConfig, shared: Arc<Shared>, outbox: Arc<Outbox>)` (never returns)

- [ ] **Step 1: Stub `discovery.rs`** so `avr` compiles

```rust
//! Finds Denon/Marantz receivers by their HEOS mDNS advertisement.

use std::net::Ipv4Addr;
use std::time::Duration;

pub async fn find(_selector: Option<&str>, _timeout: Duration) -> anyhow::Result<Ipv4Addr> {
    anyhow::bail!("discovery is not implemented")
}
```

- [ ] **Step 2: Write the failing tests** in `avr.rs`

```rust
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
        }
    }

    async fn with_daemon(
        port: u16,
        shared: Arc<Shared>,
        outbox: Arc<Outbox>,
        body: impl Future<Output = ()>,
    ) {
        tokio::select! {
            () = run(config(port), shared, outbox) => unreachable!("run returned"),
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
        with_daemon(port, shared.clone(), outbox, async {
            let (mut sock, _) = listener.accept().await.unwrap();
            let sent = read_until(&mut sock, "MU?\r").await;
            for query in QUERIES {
                assert!(sent.contains(query), "missing {query:?} in {sent:?}");
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
        with_daemon(port, shared, outbox.clone(), async {
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
        with_daemon(port, shared.clone(), outbox, async {
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
}
```

Add `pub mod avr;` and `pub mod discovery;` to `lib.rs`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p denon-volume avr`
Expected: compile errors.

- [ ] **Step 4: Implement** (above the tests)

```rust
//! The telnet session with the receiver.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time;
use tracing::{debug, info, warn};

use crate::discovery;
use crate::protocol::{self, Command, HEARTBEAT, LineBuffer, QUERIES};
use crate::scale::Mv;
use crate::state::Shared;

/// Denon's protocol documentation asks for at least 50 ms between commands.
const COMMAND_SPACING: Duration = Duration::from_millis(50);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);
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
                if let Err(e) = session(stream, &shared, &outbox).await {
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
    let stream = time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), config.port)))
        .await
        .with_context(|| format!("timed out connecting to {host}"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

async fn session(mut stream: TcpStream, shared: &Shared, outbox: &Outbox) -> anyhow::Result<()> {
    let (mut reader, mut writer) = stream.split();
    for query in QUERIES {
        writer.write_all(query.as_bytes()).await?;
        time::sleep(COMMAND_SPACING).await;
    }

    let mut lines = LineBuffer::default();
    let mut buf = [0u8; 1024];
    let mut heartbeat = time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;

    loop {
        tokio::select! {
            read = reader.read(&mut buf) => {
                let n = read?;
                anyhow::ensure!(n > 0, "receiver closed the connection");
                for line in lines.push(&buf[..n]) {
                    debug!(line, "from receiver");
                    if let Some(event) = protocol::parse_line(&line) {
                        shared.apply(event);
                    }
                }
            }
            command = outbox.next() => {
                debug!(?command, "to receiver");
                writer.write_all(command.wire().as_bytes()).await?;
                time::sleep(COMMAND_SPACING).await;
            }
            _ = heartbeat.tick() => {
                writer.write_all(HEARTBEAT.as_bytes()).await?;
            }
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p denon-volume avr`
Expected: 4 passed.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --all-targets -- --deny warnings` (expected: clean; the discovery stub's unused parameters are `_`-prefixed)

```bash
git add crates/denon-volume/src/avr.rs crates/denon-volume/src/discovery.rs crates/denon-volume/src/lib.rs
git commit -m "Add receiver telnet session with coalescing outbox"
```

---

### Task 6: `discovery` — mDNS lookup

**Files:**
- Modify: `crates/denon-volume/src/discovery.rs` (replace the stub)

**Interfaces:**
- Produces:
  - `pub fn matches(selector: &str, did: Option<&str>, fullname: &str, hostname: &str) -> bool`
  - `pub async fn find(selector: Option<&str>, timeout: Duration) -> anyhow::Result<Ipv4Addr>`

- [ ] **Step 1: Write the failing tests** (append to `discovery.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const DID: &str = "3C424E4F0005CDFEC712";
    const FULLNAME: &str = "HTAVR._heos-audio._tcp.local.";
    const HOSTNAME: &str = "HTAVR.local.";

    #[test]
    fn matches_device_id() {
        assert!(matches(DID, Some(DID), FULLNAME, HOSTNAME));
        assert!(matches(&DID.to_lowercase(), Some(DID), FULLNAME, HOSTNAME));
    }

    #[test]
    fn matches_friendly_name() {
        assert!(matches("HTAVR", Some(DID), FULLNAME, HOSTNAME));
        assert!(matches("htavr", None, FULLNAME, HOSTNAME));
    }

    #[test]
    fn rejects_other_receivers() {
        assert!(!matches("Den", Some(DID), FULLNAME, HOSTNAME));
        assert!(!matches("0005CDFEC712", Some(DID), FULLNAME, HOSTNAME));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p denon-volume discovery`
Expected: compile error (`matches` is not defined).

- [ ] **Step 3: Implement** (replace the stub, keeping the tests)

```rust
//! Finds Denon/Marantz receivers by their HEOS mDNS advertisement.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, bail};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::{info, warn};

const SERVICE_TYPE: &str = "_heos-audio._tcp.local.";

/// Whether an advertisement matches the selector: the HEOS device ID (`did` TXT record), or
/// the friendly name as the first label of the instance or host name. Case-insensitive.
pub fn matches(selector: &str, did: Option<&str>, fullname: &str, hostname: &str) -> bool {
    let first_label = |name: &str| name.split('.').next().unwrap_or_default().to_owned();
    did.is_some_and(|did| did.eq_ignore_ascii_case(selector))
        || first_label(fullname).eq_ignore_ascii_case(selector)
        || first_label(hostname).eq_ignore_ascii_case(selector)
}

/// Returns the IPv4 address of the first receiver that answers and matches `selector`.
pub async fn find(selector: Option<&str>, timeout: Duration) -> anyhow::Result<Ipv4Addr> {
    let daemon = ServiceDaemon::new().context("starting mDNS")?;
    let result = tokio::time::timeout(timeout, browse(&daemon, selector)).await;
    if let Err(e) = daemon.shutdown() {
        warn!("stopping mDNS: {e}");
    }
    result.context("no matching receiver answered over mDNS")?
}

async fn browse(daemon: &ServiceDaemon, selector: Option<&str>) -> anyhow::Result<Ipv4Addr> {
    let events = daemon.browse(SERVICE_TYPE).context("browsing mDNS")?;
    while let Ok(event) = events.recv_async().await {
        let ServiceEvent::ServiceResolved(service) = event else {
            continue;
        };
        let did = service.get_property_val_str("did");
        if let Some(selector) = selector
            && !matches(selector, did, service.get_fullname(), service.get_hostname())
        {
            continue;
        }
        if let Some(ip) = service.get_addresses_v4().into_iter().next() {
            info!(name = service.get_fullname(), %ip, "found receiver");
            return Ok(ip);
        }
    }
    bail!("mDNS browse ended")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p denon-volume discovery`
Expected: 3 passed.

- [ ] **Step 5: Live check against the real receiver** (read-only; mDNS only)

Add this temporary test at the end of the tests module:
```rust
    #[tokio::test]
    #[ignore = "needs the receiver on the LAN"]
    async fn finds_the_real_receiver() {
        let ip = find(Some("HTAVR"), Duration::from_secs(5)).await.unwrap();
        assert_eq!(ip, "10.0.0.183".parse::<Ipv4Addr>().unwrap());
    }
```
Run: `cargo test -p denon-volume discovery -- --ignored`
Expected: 1 passed. Then **delete the temporary test** so it doesn't stay tied to this LAN.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --all-targets -- --deny warnings` (expected: clean)

```bash
git add crates/denon-volume/src/discovery.rs
git commit -m "Discover the receiver over mDNS"
```

---

### Task 7: Varlink service

**Files:**
- Create: `crates/denon-volume/src/types.rs`, `crates/denon-volume/src/service.rs`, `crates/denon-volume/tests/service.rs`
- Modify: `crates/denon-volume/src/lib.rs` (add `pub mod service;` and `pub mod types;`)

**Interfaces:**
- Consumes: `state::Shared`, `avr::Outbox`, `protocol::Command`.
- Produces:
  - `types::{Capabilities, VolumeRange, VolumeStepRange, Volume, Mute, MonitorUpdate, ExternalVolumeError}`
  - `service::ExternalVolume::new(device: impl Into<String>, route: impl Into<String>, shared: Arc<Shared>, outbox: Arc<Outbox>) -> ExternalVolume`, which implements `zlink::Service` through `#[zlink::service]`.

- [ ] **Step 1: Write `types.rs`**

```rust
//! Wire types for `org.pipewire.ExternalVolume`.

use serde::{Deserialize, Serialize};
use zlink::introspect;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct VolumeRange {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct VolumeStepRange {
    pub min: f64,
    pub max: f64,
}

/// Sent as the reply parameters themselves rather than under a `capabilities` key: PipeWire's
/// client reads the flags straight out of `parameters`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub read_volume: bool,
    pub read_balance: bool,
    pub volume_range: VolumeRange,
    pub write_volume_absolute: bool,
    pub write_volume_relative: bool,
    pub write_volume_relative_step: VolumeStepRange,
    pub write_balance: bool,
    pub read_mute: bool,
    pub write_mute_value: bool,
    pub write_mute_toggle: bool,
    pub routes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct Volume {
    pub volume: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct Mute {
    pub mute: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct MonitorUpdate {
    pub route: String,
    pub volume: Vec<f64>,
    pub mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, zlink::ReplyError, introspect::ReplyError)]
#[zlink(interface = "org.pipewire.ExternalVolume")]
pub enum ExternalVolumeError {
    InvalidDevice,
    InvalidRoute,
    OperationUnsupported,
}
```

`introspect::Type` only affects what `varlinkctl introspect` shows. If the derive fails to compile on one of these types, remove it from that type and leave a note in the commit message. Behaviour is unaffected.

- [ ] **Step 2: Write the failing end-to-end tests** in `tests/service.rs`

```rust
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use denon_volume::avr::Outbox;
use denon_volume::protocol::{Command, Event};
use denon_volume::scale::{self, Mv};
use denon_volume::service::ExternalVolume;
use denon_volume::state::Shared;
use denon_volume::types::{Capabilities, ExternalVolumeError, MonitorUpdate, Mute, Volume};
use futures_util::{Stream, StreamExt, pin_mut};
use tokio::time;
use zlink::unix;

const DEVICE: &str = "alsa_card.test";
const ROUTE: &str = "hdmi-output-0";
const CAP: Mv = Mv::from_whole(70);

#[zlink::proxy("org.pipewire.ExternalVolume")]
trait ExternalVolumeProxy {
    async fn get_capabilities(
        &mut self,
        device: &str,
    ) -> zlink::Result<Result<Capabilities, ExternalVolumeError>>;
    async fn read_volume(
        &mut self,
        device: &str,
        route: &str,
    ) -> zlink::Result<Result<Volume, ExternalVolumeError>>;
    async fn read_mute(
        &mut self,
        device: &str,
        route: &str,
    ) -> zlink::Result<Result<Mute, ExternalVolumeError>>;
    async fn write_volume_absolute(
        &mut self,
        device: &str,
        route: &str,
        volume: &[f64],
    ) -> zlink::Result<Result<(), ExternalVolumeError>>;
    async fn write_mute_value(
        &mut self,
        device: &str,
        route: &str,
        mute: bool,
    ) -> zlink::Result<Result<(), ExternalVolumeError>>;
    #[zlink(more)]
    async fn monitor(
        &mut self,
        device: &str,
    ) -> zlink::Result<
        impl Stream<Item = zlink::Result<Result<MonitorUpdate, ExternalVolumeError>>>,
    >;
}

struct Harness {
    shared: Arc<Shared>,
    outbox: Arc<Outbox>,
    path: PathBuf,
}

impl Harness {
    fn receiver_on_pc(&self) {
        self.shared.set_connected(true);
        self.shared.apply(Event::Power(true));
        self.shared.apply(Event::Input("BD".into()));
        self.shared.apply(Event::Volume(Mv::from_whole(62)));
        self.shared.apply(Event::Mute(false));
    }
}

async fn with_service<F, Fut>(body: F)
where
    F: FnOnce(Harness) -> Fut,
    Fut: Future<Output = ()>,
{
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ev.sock");
    let shared = Arc::new(Shared::new("BD", CAP));
    let outbox = Arc::new(Outbox::default());
    let service = ExternalVolume::new(DEVICE, ROUTE, shared.clone(), outbox.clone());
    let server = zlink::Server::new(unix::bind(&path).unwrap(), service);
    let harness = Harness { shared, outbox, path };
    tokio::select! {
        result = server.run() => panic!("server exited: {result:?}"),
        () = body(harness) => {}
    }
}

async fn next_update(
    stream: &mut (impl Stream<Item = zlink::Result<Result<MonitorUpdate, ExternalVolumeError>>> + Unpin),
) -> MonitorUpdate {
    time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("no monitor update")
        .expect("stream ended")
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn advertises_absolute_volume_and_mute() {
    with_service(|h| async move {
        h.receiver_on_pc();
        let mut conn = unix::connect(&h.path).await.unwrap();
        let caps = conn.get_capabilities(DEVICE).await.unwrap().unwrap();
        assert!(caps.read_volume && caps.write_volume_absolute);
        assert!(caps.read_mute && caps.write_mute_value);
        assert!(!caps.write_volume_relative);
        assert_eq!(caps.routes, [ROUTE]);
        assert_eq!(
            conn.get_capabilities("other").await.unwrap(),
            Err(ExternalVolumeError::InvalidDevice)
        );
    })
    .await;
}

#[tokio::test]
async fn reads_need_a_known_state() {
    with_service(|h| async move {
        let mut conn = unix::connect(&h.path).await.unwrap();
        assert_eq!(
            conn.read_volume(DEVICE, ROUTE).await.unwrap(),
            Err(ExternalVolumeError::OperationUnsupported)
        );
        h.receiver_on_pc();
        let volume = conn.read_volume(DEVICE, ROUTE).await.unwrap().unwrap();
        assert_eq!(volume.volume, [scale::from_mv(Mv::from_whole(62), CAP)]);
        assert!(!conn.read_mute(DEVICE, ROUTE).await.unwrap().unwrap().mute);
        assert_eq!(
            conn.read_volume(DEVICE, "hdmi-output-1").await.unwrap(),
            Err(ExternalVolumeError::InvalidRoute)
        );
    })
    .await;
}

#[tokio::test]
async fn writes_on_the_pc_input_reach_the_outbox() {
    with_service(|h| async move {
        h.receiver_on_pc();
        let mut conn = unix::connect(&h.path).await.unwrap();
        conn.write_volume_absolute(DEVICE, ROUTE, &[0.9, 0.9])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.outbox.take(), Some(Command::Volume(Mv::from_half_steps(135))));
        conn.write_mute_value(DEVICE, ROUTE, true).await.unwrap().unwrap();
        assert_eq!(h.outbox.take(), Some(Command::Mute(true)));

        h.shared.apply(Event::Volume(Mv::from_half_steps(135)));
        let volume = conn.read_volume(DEVICE, ROUTE).await.unwrap().unwrap();
        assert_eq!(volume.volume, [0.9]);
    })
    .await;
}

#[tokio::test]
async fn writes_elsewhere_are_dropped_and_snap_back() {
    with_service(|h| async move {
        h.receiver_on_pc();
        h.shared.apply(Event::Input("GAME".into()));
        let mut monitor_conn = unix::connect(&h.path).await.unwrap();
        let mut conn = unix::connect(&h.path).await.unwrap();
        let stream = monitor_conn.monitor(DEVICE).await.unwrap();
        pin_mut!(stream);
        let real = next_update(&mut stream).await;

        conn.write_volume_absolute(DEVICE, ROUTE, &[1.0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.outbox.take(), None);
        assert_eq!(next_update(&mut stream).await, real);
    })
    .await;
}

#[tokio::test]
async fn monitor_streams_receiver_changes() {
    with_service(|h| async move {
        let mut conn = unix::connect(&h.path).await.unwrap();
        let stream = conn.monitor(DEVICE).await.unwrap();
        pin_mut!(stream);

        h.receiver_on_pc();
        let first = next_update(&mut stream).await;
        assert_eq!(first.route, ROUTE);
        assert_eq!(first.volume, [scale::from_mv(Mv::from_whole(62), CAP)]);

        h.shared.apply(Event::Volume(Mv::from_whole(60)));
        let update = next_update(&mut stream).await;
        assert_eq!(update.volume, [scale::from_mv(Mv::from_whole(60), CAP)]);

        h.shared.apply(Event::Mute(true));
        assert_eq!(next_update(&mut stream).await.mute, Some(true));
    })
    .await;
}
```

In `monitor_streams_receiver_changes`, the first update is the 62 level, not the empty initial state. The test checks that Monitor holds back until the level is known; it may still see intermediate updates while `receiver_on_pc` applies events one at a time. If it does, change the first assertion to loop on `next_update` until `volume` is non-empty and then check the value. Don't weaken the check that no update has an empty `volume`.

Add `pub mod service;` and `pub mod types;` to `lib.rs`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p denon-volume --test service`
Expected: compile error (`denon_volume::service` doesn't exist).

- [ ] **Step 4: Implement `service.rs`**

```rust
//! The `org.pipewire.ExternalVolume` Varlink service.

use std::future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, warn};
use zlink::{Reply, service};

use crate::avr::Outbox;
use crate::protocol::Command;
use crate::state::Shared;
use crate::types::{
    Capabilities, ExternalVolumeError, MonitorUpdate, Mute, Volume, VolumeRange, VolumeStepRange,
};

/// PipeWire reads the volume right after asking for capabilities, and gives that first call
/// 2 s. Waiting a little for the receiver to report in means that read gets a real answer.
const FIRST_STATE_WAIT: Duration = Duration::from_millis(1500);

pub struct ExternalVolume {
    device: String,
    route: String,
    shared: Arc<Shared>,
    outbox: Arc<Outbox>,
}

impl ExternalVolume {
    pub fn new(
        device: impl Into<String>,
        route: impl Into<String>,
        shared: Arc<Shared>,
        outbox: Arc<Outbox>,
    ) -> Self {
        Self {
            device: device.into(),
            route: route.into(),
            shared,
            outbox,
        }
    }

    fn check_device(&self, device: &str) -> Result<(), ExternalVolumeError> {
        if device == self.device {
            Ok(())
        } else {
            warn!(device, expected = %self.device, "call for an unknown device");
            Err(ExternalVolumeError::InvalidDevice)
        }
    }

    fn check(&self, device: &str, route: &str) -> Result<(), ExternalVolumeError> {
        self.check_device(device)?;
        if route == self.route {
            Ok(())
        } else {
            warn!(route, expected = %self.route, "call for an unknown route");
            Err(ExternalVolumeError::InvalidRoute)
        }
    }

    fn write_mute(&self, mute: bool) {
        if self.shared.request_mute(mute) {
            self.outbox.push(Command::Mute(mute));
        } else {
            debug!(mute, "receiver isn't on the PC input; ignoring mute write");
        }
    }
}

#[service(
    interface = "org.pipewire.ExternalVolume",
    product = "denon-volume",
    version = env!("CARGO_PKG_VERSION")
)]
impl ExternalVolume {
    #[zlink(rename = "GetCapabilities")]
    async fn get_capabilities(&self, device: &str) -> Result<Capabilities, ExternalVolumeError> {
        self.check_device(device)?;
        let mut rx = self.shared.subscribe();
        let known = rx.wait_for(|s| s.mv.is_some() && s.mute.is_some());
        if tokio::time::timeout(FIRST_STATE_WAIT, known).await.is_err() {
            warn!("receiver state still unknown; PipeWire's first read will fail");
        }
        Ok(Capabilities {
            read_volume: true,
            read_balance: false,
            volume_range: VolumeRange {
                min: 0.0,
                max: 1.0,
                step: 0.0,
            },
            write_volume_absolute: true,
            write_volume_relative: false,
            write_volume_relative_step: VolumeStepRange { min: 0.0, max: 0.0 },
            write_balance: false,
            read_mute: true,
            write_mute_value: true,
            write_mute_toggle: true,
            routes: vec![self.route.clone()],
        })
    }

    #[zlink(rename = "ReadVolume")]
    async fn read_volume(&self, device: &str, route: &str) -> Result<Volume, ExternalVolumeError> {
        self.check(device, route)?;
        let volume = self
            .shared
            .snapshot()
            .volume(self.shared.cap())
            .ok_or(ExternalVolumeError::OperationUnsupported)?;
        Ok(Volume {
            volume: vec![volume],
        })
    }

    #[zlink(rename = "ReadMute")]
    async fn read_mute(&self, device: &str, route: &str) -> Result<Mute, ExternalVolumeError> {
        self.check(device, route)?;
        let mute = self
            .shared
            .snapshot()
            .mute
            .ok_or(ExternalVolumeError::OperationUnsupported)?;
        Ok(Mute { mute })
    }

    #[zlink(rename = "WriteVolumeAbsolute")]
    async fn write_volume_absolute(
        &self,
        device: &str,
        route: &str,
        volume: Vec<f64>,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        let Some(v) = volume.into_iter().reduce(f64::max) else {
            return Ok(());
        };
        match self.shared.request_volume(v) {
            Some(mv) => self.outbox.push(Command::Volume(mv)),
            None => debug!(volume = v, "receiver isn't on the PC input; ignoring volume write"),
        }
        Ok(())
    }

    #[zlink(rename = "WriteVolumeRelative")]
    async fn write_volume_relative(
        &self,
        device: &str,
        route: &str,
        step: f64,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        debug!(step, "relative volume isn't advertised");
        Err(ExternalVolumeError::OperationUnsupported)
    }

    #[zlink(rename = "WriteMuteValue")]
    async fn write_mute_value(
        &self,
        device: &str,
        route: &str,
        mute: bool,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        self.write_mute(mute);
        Ok(())
    }

    #[zlink(rename = "WriteMuteToggle")]
    async fn write_mute_toggle(&self, device: &str, route: &str) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        let muted = self.shared.snapshot().mute.unwrap_or(false);
        self.write_mute(!muted);
        Ok(())
    }

    #[zlink(rename = "Monitor", more)]
    async fn monitor(
        &self,
        more: bool,
        device: String,
    ) -> impl Stream<Item = Reply<MonitorUpdate>> + use<> {
        if device != self.device {
            warn!(device, expected = %self.device, "monitor for an unknown device");
        }
        let route = self.route.clone();
        let cap = self.shared.cap();
        WatchStream::new(self.shared.subscribe())
            .filter(|s| future::ready(s.mv.is_some()))
            .take(if more { usize::MAX } else { 1 })
            .map(move |s| {
                let update = MonitorUpdate {
                    route: route.clone(),
                    volume: s.volume(cap).into_iter().collect(),
                    mute: s.mute,
                };
                Reply::new(Some(update)).set_continues(more.then_some(true))
            })
    }
}
```

- [ ] **Step 5: Run all tests to verify they pass**

Run: `cargo test -p denon-volume`
Expected: all unit tests plus 5 in `tests/service.rs` pass.

- [ ] **Step 6: Lint and commit**

Run: `cargo clippy --all-targets -- --deny warnings` (expected: clean)

```bash
git add crates/denon-volume/src/types.rs crates/denon-volume/src/service.rs crates/denon-volume/src/lib.rs crates/denon-volume/tests/service.rs
git commit -m "Serve org.pipewire.ExternalVolume"
```

---

### Task 8: CLI, socket activation and a live smoke test

**Files:**
- Modify: `crates/denon-volume/src/main.rs` (replace the placeholder)

**Interfaces:**
- Consumes: everything above.
- Produces: the `denon-volume` binary with flags `--device`, `--route` (default `hdmi-output-0`), `--pc-input`, `--cap-mv` (default `70`, steps of 0.5), `--host`, `--select` (conflicts with `--host`), `--socket`.

- [ ] **Step 1: Write the failing test for `--cap-mv` parsing** at the bottom of `main.rs`

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p denon-volume --bin denon-volume`
Expected: compile error (`parse_cap` is not defined).

- [ ] **Step 3: Implement `main.rs`** (above the tests)

```rust
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
        anyhow::ensure!(meta.file_type().is_socket(), "{path:?} exists and isn't a socket");
        std::fs::remove_file(path)?;
    }
    zlink::unix::bind(path).with_context(|| format!("binding {path:?}"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
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
```

- [ ] **Step 4: Run the tests and lint**

Run: `cargo test -p denon-volume && cargo clippy --all-targets -- --deny warnings`
Expected: all tests pass; no warnings.

- [ ] **Step 5: Live read-only smoke test against the receiver**

The receiver accepts one telnet client at a time, so make sure nothing else holds port 23 (e.g. Home Assistant). Start the daemon:

```bash
cargo run -- --device alsa_card.pci-0000_01_00.1 --pc-input BD \
  --socket "$XDG_RUNTIME_DIR/denon-volume-test.sock"
```
Expected log: `found receiver name=HTAVR._heos-audio._tcp.local. ip=10.0.0.183`, then `connected to receiver`.

In another terminal:
```bash
S=unix:$XDG_RUNTIME_DIR/denon-volume-test.sock; I=org.pipewire.ExternalVolume
varlinkctl call $S $I.GetCapabilities '{"device":"alsa_card.pci-0000_01_00.1"}'
varlinkctl call $S $I.ReadVolume '{"device":"alsa_card.pci-0000_01_00.1","route":"hdmi-output-0"}'
varlinkctl call $S $I.ReadMute '{"device":"alsa_card.pci-0000_01_00.1","route":"hdmi-output-0"}'
varlinkctl introspect $S $I
```
Expected: capabilities with `readVolume: true` etc.; a volume that matches the receiver's display (`from_mv`; e.g. MV62 → ≈0.7356); `mute: false`; and an IDL listing all eight methods.

Then run `varlinkctl call --more $S $I.Monitor '{"device":"alsa_card.pci-0000_01_00.1"}'` and **ask the user to turn the receiver's volume knob a couple of clicks**. Expected: one JSON line per click. **Do not issue write calls without the user's go-ahead**, because they change the receiver's real volume. If the user approves, write the level that `ReadVolume` just returned (a no-op): `varlinkctl call $S $I.WriteVolumeAbsolute '{"device":"…","route":"hdmi-output-0","volume":[<that value>]}'`.

Stop the daemon with Ctrl-C and confirm the socket file is removed.

- [ ] **Step 6: Commit**

```bash
git add crates/denon-volume/src/main.rs
git commit -m "Add denon-volume CLI with socket activation"
```

---

### Task 9: PipeWire patches — vendor, fix, add Monitor

**Files:**
- Create: `patches/README.md`, `patches/pipewire/0001…0018-*.patch` (copied), `patches/wireplumber/0001-*.patch` (copied), `patches/pipewire/0019-spa-alsa-Fix-external-volume-reads-and-writes.patch`, `patches/pipewire/0020-spa-alsa-Follow-external-volume-changes-via-Monitor.patch`, `nix/patched.nix`
- Modify: `flake.nix` (add `packages.pipewire`, `packages.wireplumber`, `flake.lib`)

**Interfaces:**
- Produces:
  - `nix/patched.nix`: `{ pkgs, pipewirePatches, wireplumberPatches }: { pipewire; wireplumber; }`
  - Flake outputs: `lib.pipewirePatches`, `lib.wireplumberPatches` (sorted lists of paths), `packages.<system>.pipewire`, `packages.<system>.wireplumber`

- [ ] **Step 1: Vendor Valve's patches**

```bash
SRC=$(mktemp -d) && tar -xzf ~/Downloads/pipewire-external-volume.tar.gz -C "$SRC"
mkdir -p patches/pipewire patches/wireplumber
cp "$SRC"/pipewire-external-volume/patches/pipewire/*.patch patches/pipewire/
cp "$SRC"/pipewire-external-volume/patches/wireplumber/*.patch patches/wireplumber/
ls patches/pipewire | wc -l   # expect 18
```

Write `patches/README.md`:
```markdown
# Patches

`pipewire/0001`–`0018` and `wireplumber/0001` are Valve/Collabora's external volume control
series, taken unmodified from SteamOS source packages (`jupiter-main/pipewire-1:1.6.8-1.5`,
`jupiter-main/wireplumber-0.5.18-1.1`) and rebased onto PipeWire 1.6.9. They are not upstream.

`pipewire/0019` fixes reply parsing and write handling in that series, and `pipewire/0020`
makes ACP follow volume changes pushed by the service through `Monitor`.
```

- [ ] **Step 2: Prepare a PipeWire tree with the series applied**

```bash
mkdir -p .work && git clone --depth 1 --branch 1.6.9 https://gitlab.freedesktop.org/pipewire/pipewire.git .work/pipewire
cd .work/pipewire && git am ../../patches/pipewire/00*.patch && git log --oneline -1
```
Expected: the HEAD subject is `spa: varlink: Set a default read timeout for sync calls`.

- [ ] **Step 3: 0019 — fix varlink reply parsing** in `.work/pipewire/spa/plugins/support/varlink.c`

Replace the whole `process_message` function with:

```c
static void
process_message(struct client *this, const char *msg, size_t len)
{
	struct impl *impl = this->impl;
	struct spa_json json;
	char key[64];
	const char *val, *error = NULL, *params = NULL;
	int res, error_len = 0, params_len = 0;
	bool continues = false;

	spa_log_trace(impl->log, "got message: %.*s", (int)len, msg);

	if (spa_json_begin_object(&json, msg, len) <= 0)
		goto error;

	/* Reply keys may arrive in any order, so read them all before
	 * reporting; "continues" commonly follows "parameters". */
	while ((res = spa_json_object_next(&json, key, sizeof(key), &val)) > 0) {
		if (spa_streq(key, "error")) {
			error = val;
			error_len = res;
		} else if (spa_streq(key, "continues")) {
			if (spa_json_parse_bool(val, res, &continues) < 0)
				goto error;
		} else if (spa_streq(key, "parameters")) {
			params = val;
			params_len = spa_json_container_len(&json, val, res);
		}
	}

	if (this->cb)
		this->cb(this->cb_userdata, params, error,
				error ? error_len : params_len, continues);

	if (!this->more_pending || !continues) {
		/* We didn't ask for more, but server might send */
		if (continues)
			spa_log_warn(impl->log, "Unexpected continues");

		this->reply_pending = false;
		this->more_pending = false;
		this->cb = NULL;
		this->cb_userdata = NULL;
	}

	return;

error:
	spa_log_error(impl->log, "Could not parse: %.*s", (int)len, msg);
}
```

- [ ] **Step 4: 0019 — fix reads and make writes oneway** in `.work/pipewire/spa/plugins/alsa/acp/ext-volume.c`

1. Delete the `check_reply` function.
2. Add these helpers directly after the `client_events` definition:

```c
/* Points params at the reply's "parameters" object and returns its length,
 * or a negative errno for an error reply. */
static int reply_parameters(const char *reply, int len, const char **params)
{
	struct spa_json it;
	char key[64];
	const char *val, *found = NULL;
	int l, found_len = 0;

	if (spa_json_begin_object(&it, reply, len) <= 0)
		return -EINVAL;

	while ((l = spa_json_object_next(&it, key, sizeof(key), &val)) > 0) {
		if (spa_streq(key, "error")) {
			pa_log_error("External volume control error: %.*s", l, val);
			return -EIO;
		}
		if (spa_streq(key, "parameters") && spa_json_is_object(val, l)) {
			found = val;
			found_len = spa_json_container_len(&it, val, l);
		}
	}

	if (!found)
		return -ENOENT;

	*params = found;
	return found_len;
}

/* Reads the "volume" float array, scaled so that 1.0 is PA_VOLUME_NORM. */
static int parse_volume(const char *params, int len, pa_cvolume *cvol)
{
	struct spa_json it, arr;
	char key[64];
	const char *val;
	float v;
	int l;

	if (spa_json_begin_object(&it, params, len) <= 0)
		return -EINVAL;

	while ((l = spa_json_object_next(&it, key, sizeof(key), &val)) > 0) {
		if (!spa_streq(key, "volume") || !spa_json_is_array(val, l))
			continue;

		l = spa_json_container_len(&it, val, l);
		if (spa_json_begin_array(&arr, val, l) <= 0)
			return -EINVAL;

		cvol->channels = 0;
		while (cvol->channels < PA_CHANNELS_MAX && spa_json_get_float(&arr, &v) > 0)
			cvol->values[cvol->channels++] = lrint(v * PA_VOLUME_NORM);

		return cvol->channels > 0 ? 0 : -EINVAL;
	}

	return -ENOENT;
}

static int parse_mute(const char *params, int len, bool *mute)
{
	struct spa_json it;
	char key[64];
	const char *val;
	int l;

	if (spa_json_begin_object(&it, params, len) <= 0)
		return -EINVAL;

	while ((l = spa_json_object_next(&it, key, sizeof(key), &val)) > 0) {
		if (spa_streq(key, "mute"))
			return spa_json_parse_bool(val, l, mute) < 0 ? -EINVAL : 0;
	}

	return -ENOENT;
}
```

3. In `spa_acp_ext_volume_read_volume`, keep the params-building block, then replace everything from the `spa_varlink_client_call_sync(...)` line to the end of the function with the code below. Also remove the now-unused locals `reply_json`, `key` and `str`, and add `const char *reply_params;`.

```c
	res = spa_varlink_client_call_sync(ext_volume->client, METHOD_READ_VOLUME, params,
			&reply, DEFAULT_TIMEOUT_MSEC);
	if (res < 0) {
		pa_log_error("Failed to query volume : %s", spa_strerror(res));
		return res;
	}

	if ((res = reply_parameters(reply, res, &reply_params)) < 0 ||
			(res = parse_volume(reply_params, res, cvol)) < 0) {
		pa_log_error("Could not read volume");
		return res;
	}

	return 0;
}
```

4. In `spa_acp_ext_volume_read_mute`, make the same change: keep the params block, remove `reply_json`, `key` and `str`, add `const char *reply_params;`, and end the function with:

```c
	res = spa_varlink_client_call_sync(ext_volume->client, METHOD_READ_MUTE, params,
			&reply, DEFAULT_TIMEOUT_MSEC);
	if (res < 0) {
		pa_log_error("Failed to query mute state: %s", spa_strerror(res));
		return res;
	}

	if ((res = reply_parameters(reply, res, &reply_params)) < 0 ||
			(res = parse_mute(reply_params, res, mute)) < 0) {
		pa_log_error("Could not read mute");
		return res;
	}

	return 0;
}
```

5. In each of the four write functions, change the `spa_varlink_client_call(...)` arguments from `params, false, false, check_reply, METHOD_…` to `params, true, false, NULL, NULL`. Above the first write function, add:

```c
/* Writes are sent oneway: the client allows a single outstanding reply per
 * connection, so waiting for acknowledgements would make writes issued in
 * quick succession, as from a dragged slider, fail. */
```

6. Change the `cvol` parameter of `spa_acp_ext_volume_write_volume_absolute` to `const pa_cvolume *cvol`, in both `ext-volume.c` and `ext-volume.h`.

- [ ] **Step 5: 0019 — write the new volume and init once per card** in `.work/pipewire/spa/plugins/alsa/acp/acp.c`

1. In `set_volume`, change the absolute-write call's last argument from `&dev->real_volume` to `v`.
2. Replace `init_ext_volume` with:

```c
static void init_ext_volume(pa_card *impl)
{
	struct acp_card *card = &impl->card;
	int res;

	if (!impl->ext_volume_path)
		return;

	/* Called for every enabled device, but the service connection is
	 * per card. */
	if (card->ext_volume.client)
		return;

	if (!acp_varlink) {
		pa_log_error("External volume control requires support.varlink = true");
		return;
	}

	res = spa_acp_ext_volume_init(&card->ext_volume, acp_varlink, impl->ext_volume_path, impl->name, NULL);
	if (res < 0)
		spa_acp_ext_volume_destroy(&card->ext_volume);
}
```

- [ ] **Step 6: Build-check 0019 and export it**

Run (in `.work/pipewire`): `nix-shell -p meson ninja pkg-config alsa-lib dbus --run 'meson setup build -Dauto_features=disabled -Dalsa=enabled -Dspa-plugins=enabled -Dsupport=enabled -Dsession-managers=[] -Dtests=disabled -Dexamples=disabled && ninja -C build'`
Expected: builds without warnings in `ext-volume.c`, `acp.c` or `varlink.c`. If a meson option name differs on this version, check `meson configure build` for the actual names. A full build in Step 9 is the backstop.

```bash
git add -A && git commit -m "spa: alsa: Fix external volume reads and writes" -m "ReadVolume and ReadMute read their value from the wrong JSON iterator, the varlink client stopped parsing before \"continues\", absolute volume writes sent the previous volume, and a second write while one awaited its reply failed. Writes are now oneway, and the service connection is set up once per card."
git format-patch -1 --start-number 19 -o ../../patches/pipewire
cd ../..
```

- [ ] **Step 7: 0020 — Monitor support in `ext-volume.h`**

In `.work/pipewire/spa/plugins/alsa/acp/ext-volume.h`, add `#include <spa/support/loop.h>`, replace the struct, and add the typedef and declaration:

```c
typedef void (*spa_acp_ext_volume_changed_t)(void *data, const char *route,
		const pa_cvolume *volume, const bool *mute);

struct spa_acp_ext_volume {
	struct spa_varlink *varlink;
	struct spa_loop_utils *loop_utils;
	char *path;
	char *device;

	struct spa_varlink_client *client;
	struct spa_hook listener;

	struct spa_varlink_client *monitor_client;
	struct spa_hook monitor_listener;
	struct spa_source *reconnect_timer;
	spa_acp_ext_volume_changed_t changed;
	void *changed_data;

	enum spa_audio_volume_control_flags flags;
};

/* Follows volume and mute changes pushed by the service, reconnecting if the
 * service goes away. volume and mute are NULL when an update omits them. */
int spa_acp_ext_volume_monitor(struct spa_acp_ext_volume *ext_volume,
		struct spa_loop_utils *loop_utils, spa_acp_ext_volume_changed_t changed,
		void *data);
```

- [ ] **Step 8: 0020 — Monitor support in `ext-volume.c`**

1. Add `#include <stdlib.h>`, `#include <string.h>` and `#include <time.h>`, plus `#define METHOD_MONITOR VOLUME_CONTROL_BASE "Monitor"` and `#define RECONNECT_MSEC 1000`.

2. Replace `client_disconnect` with these two functions, placed before `client_events`:

```c
static void schedule_reconnect(struct spa_acp_ext_volume *ext_volume)
{
	struct timespec value = {
		.tv_sec = RECONNECT_MSEC / 1000,
		.tv_nsec = (RECONNECT_MSEC % 1000) * SPA_NSEC_PER_MSEC,
	};

	if (ext_volume->loop_utils && ext_volume->reconnect_timer)
		spa_loop_utils_update_timer(ext_volume->loop_utils,
				ext_volume->reconnect_timer, &value, NULL, false);
}

static void client_disconnect(void *userdata)
{
	struct spa_acp_ext_volume *ext_volume = userdata;

	pa_log_warn("Lost connection to external volume control at %s", ext_volume->path);
	/* The client can't be torn down from inside its own callback, so the
	 * reconnect happens on a timer. */
	schedule_reconnect(ext_volume);
}
```

3. After `parse_mute`, add the Monitor machinery:

```c
static void monitor_reply(void *data, const char *params, const char *error,
		size_t len, bool continues)
{
	struct spa_acp_ext_volume *ext_volume = data;
	struct spa_json it;
	char key[64], route[256] = "";
	const char *val;
	pa_cvolume volume;
	bool mute;
	int l;

	if (error) {
		pa_log_error("External volume monitor failed: %.*s", (int)len, error);
		schedule_reconnect(ext_volume);
		return;
	}
	if (!params || !ext_volume->changed)
		return;

	if (spa_json_begin_object(&it, params, len) > 0) {
		while ((l = spa_json_object_next(&it, key, sizeof(key), &val)) > 0) {
			if (spa_streq(key, "route"))
				spa_json_parse_stringn(val, l, route, sizeof(route));
		}
	}

	ext_volume->changed(ext_volume->changed_data, route,
			parse_volume(params, len, &volume) == 0 ? &volume : NULL,
			parse_mute(params, len, &mute) == 0 ? &mute : NULL);
}

static void close_clients(struct spa_acp_ext_volume *ext_volume)
{
	if (ext_volume->monitor_client) {
		spa_hook_remove(&ext_volume->monitor_listener);
		spa_varlink_client_destroy(ext_volume->monitor_client);
		ext_volume->monitor_client = NULL;
	}
	if (ext_volume->client) {
		spa_hook_remove(&ext_volume->listener);
		spa_varlink_client_destroy(ext_volume->client);
		ext_volume->client = NULL;
	}
}

/* Monitor gets its own connection: a pending "more" call blocks the sync
 * reads ACP makes on the main one. */
static int start_monitor(struct spa_acp_ext_volume *ext_volume)
{
	struct spa_json_builder params_json;
	spa_autofree char *params = NULL;
	size_t len;

	ext_volume->monitor_client = spa_varlink_connect(ext_volume->varlink, ext_volume->path);
	if (!ext_volume->monitor_client)
		return -ECONNREFUSED;

	spa_varlink_client_add_listener(ext_volume->monitor_client,
			&ext_volume->monitor_listener, &client_events, ext_volume);

	spa_json_builder_memstream(&params_json, &params, &len, 0);
	spa_json_builder_object_push(&params_json, NULL, "{");
	spa_json_builder_object_string(&params_json, "device", ext_volume->device);
	spa_json_builder_pop(&params_json, "}");
	spa_json_builder_close(&params_json);

	return spa_varlink_client_call(ext_volume->monitor_client, METHOD_MONITOR, params,
			false, true, monitor_reply, ext_volume);
}

static void on_reconnect_timer(void *data, uint64_t expirations)
{
	struct spa_acp_ext_volume *ext_volume = data;

	close_clients(ext_volume);

	ext_volume->client = spa_varlink_connect(ext_volume->varlink, ext_volume->path);
	if (!ext_volume->client) {
		schedule_reconnect(ext_volume);
		return;
	}
	spa_varlink_client_add_listener(ext_volume->client, &ext_volume->listener,
			&client_events, ext_volume);

	if (start_monitor(ext_volume) < 0) {
		schedule_reconnect(ext_volume);
		return;
	}

	pa_log_info("Reconnected to external volume control at %s", ext_volume->path);
}
```

4. At the start of `spa_acp_ext_volume_init`, right after the "Connecting to external volume control" log line, add:

```c
	ext_volume->varlink = varlink;
	ext_volume->path = strdup(path);
	ext_volume->device = strdup(device ? device : "");
```

5. Replace `spa_acp_ext_volume_destroy` with:

```c
void
spa_acp_ext_volume_destroy(struct spa_acp_ext_volume *ext_volume)
{
	if (ext_volume->reconnect_timer) {
		spa_loop_utils_destroy_source(ext_volume->loop_utils, ext_volume->reconnect_timer);
		ext_volume->reconnect_timer = NULL;
	}
	close_clients(ext_volume);
	free(ext_volume->path);
	ext_volume->path = NULL;
	free(ext_volume->device);
	ext_volume->device = NULL;
	ext_volume->flags = 0;
}
```

6. Append:

```c
int
spa_acp_ext_volume_monitor(struct spa_acp_ext_volume *ext_volume,
		struct spa_loop_utils *loop_utils, spa_acp_ext_volume_changed_t changed,
		void *data)
{
	int res;

	ext_volume->changed = changed;
	ext_volume->changed_data = data;
	ext_volume->loop_utils = loop_utils;

	if (loop_utils)
		ext_volume->reconnect_timer = spa_loop_utils_add_timer(loop_utils,
				on_reconnect_timer, ext_volume);
	else
		pa_log_warn("No loop utils; external volume control won't reconnect");

	if ((res = start_monitor(ext_volume)) < 0)
		pa_log_error("Could not monitor external volume: %s", spa_strerror(res));

	return res;
}
```

- [ ] **Step 9: 0020 — apply updates in `acp.c` and pass loop utils**

1. In `acp.c`, below `struct spa_varlink *acp_varlink;`, add `struct spa_loop_utils *acp_loop_utils;`.

2. In `alsa-acp-device.c`, next to `extern struct spa_varlink *acp_varlink;`, add `extern struct spa_loop_utils *acp_loop_utils;`. After the `acp_varlink = spa_support_find(...)` line, add `acp_loop_utils = spa_support_find(support, n_support, SPA_TYPE_INTERFACE_LoopUtils);`.

3. In `acp.c`, directly above `init_ext_volume`, add:

```c
/* Applies a volume reported by the external service, expanding a single value
 * to every channel. */
static void update_ext_volume(pa_alsa_device *dev, const pa_cvolume *ext_vol)
{
	pa_card *impl = dev->card;
	pa_cvolume r;
	uint32_t i;

	if (ext_vol->channels == 1) {
		r.channels = dev->device.format.channels;
		for (i = 0; i < r.channels; i++)
			r.values[i] = ext_vol->values[0];
	} else if (ext_vol->channels == dev->device.format.channels) {
		r = *ext_vol;
	} else {
		pa_log_error("Mismatch channel count: device %u != volume %u",
				dev->device.format.channels, ext_vol->channels);
		return;
	}

	if (pa_cvolume_equal(&dev->hardware_volume, &r))
		return;

	dev->real_volume = dev->hardware_volume = r;
	pa_cvolume_reset(&dev->soft_volume, r.channels);
	pa_log_info("New external volume: %d", pa_cvolume_max(&r));

	if (impl->events && impl->events->volume_changed)
		impl->events->volume_changed(impl->user_data, &dev->device);
}

static void update_ext_mute(pa_alsa_device *dev, bool mute)
{
	pa_card *impl = dev->card;

	if (mute == dev->muted)
		return;

	dev->muted = mute;
	pa_log_info("New external mute: %d", mute);

	if (impl->events && impl->events->mute_changed)
		impl->events->mute_changed(impl->user_data, &dev->device);
}

static void ext_volume_changed(void *data, const char *route,
		const pa_cvolume *volume, const bool *mute)
{
	pa_card *impl = data;
	pa_alsa_device *dev;
	uint32_t idx;

	PA_DYNARRAY_FOREACH(dev, &impl->out.devices, idx) {
		if (!dev->active_port || !spa_streq(dev->active_port->name, route))
			continue;
		if (volume)
			update_ext_volume(dev, volume);
		if (mute)
			update_ext_mute(dev, *mute);
	}
}
```

4. At the end of `init_ext_volume`, replace the trailing `if (res < 0) spa_acp_ext_volume_destroy(...);` with:

```c
	if (res < 0) {
		spa_acp_ext_volume_destroy(&card->ext_volume);
		return;
	}

	if (card->ext_volume.flags & (SPA_AUDIO_VOLUME_CONTROL_READ_VOLUME |
				SPA_AUDIO_VOLUME_CONTROL_READ_MUTE))
		spa_acp_ext_volume_monitor(&card->ext_volume, acp_loop_utils,
				ext_volume_changed, impl);
```

5. In `read_volume`, replace the body of the external branch after the successful `spa_acp_ext_volume_read_volume` call (the `FIXME` comment and the channel-expansion `if/else`) with:

```c
		update_ext_volume(dev, &ext_vol);
		return 0;
```

6. In `read_mute`, after the successful `spa_acp_ext_volume_read_mute` call in the external branch, add:

```c
		update_ext_mute(dev, mute);
		return 0;
```

7. In `set_volume`, replace the external-branch lines `if (v != &dev->real_volume) dev->real_volume = *v;` (and their comment) with:

```c
			/* The service reports this level back through Monitor once
			 * the receiver applies it; recording it as the hardware
			 * volume makes that echo a no-op. */
			dev->real_volume = dev->hardware_volume = *v;
```

- [ ] **Step 10: Build-check 0020 and export it**

Run (in `.work/pipewire`): `nix-shell -p meson ninja pkg-config alsa-lib dbus --run 'ninja -C build'`
Expected: builds without new warnings.

```bash
git add -A && git commit -m "spa: alsa: Follow external volume changes via Monitor" -m "ACP opens a second connection for the streaming Monitor call and applies the pushed volume and mute to the active route, so changes made on the device itself show up in PipeWire. Both connections are re-established on a timer if the service goes away."
git format-patch -1 --start-number 20 -o ../../patches/pipewire
cd ../..
```

- [ ] **Step 11: Write `nix/patched.nix`**

```nix
{
  pkgs,
  pipewirePatches,
  wireplumberPatches,
}:
rec {
  pipewire = pkgs.pipewire.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ pipewirePatches;
  });

  # WirePlumber loads the SPA plugins and libspa-varlink from the PipeWire it was
  # built against, so it has to be rebuilt on the patched one.
  wireplumber = (pkgs.wireplumber.override { inherit pipewire; }).overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ wireplumberPatches;
  });
}
```

- [ ] **Step 12: Wire the patches into `flake.nix`**

Inside `mkFlake { … } { … }`, add a top-level `flake` attribute next to `perSystem`:

```nix
      flake.lib =
        let
          patchesIn =
            dir: map (name: dir + "/${name}") (builtins.attrNames (builtins.readDir dir));
        in
        {
          pipewirePatches = patchesIn ./patches/pipewire;
          wireplumberPatches = patchesIn ./patches/wireplumber;
        };
```

In `perSystem`, add `let patched = import ./nix/patched.nix { inherit pkgs; inherit (inputs.self.lib) pipewirePatches wireplumberPatches; };` and extend `packages` with `inherit (patched) pipewire wireplumber;`. Change the `perSystem` argument list to `{ config, pkgs, ... }` if needed (it already is).

- [ ] **Step 13: Build the patched packages**

Run: `git add -A && nix build .#pipewire .#wireplumber -L`
Expected: both build. The PipeWire build log shows all 20 patches applied with no fuzz. If WirePlumber's patch fails on the pinned nixpkgs version, report the version and the failing hunk instead of editing Valve's patch.

- [ ] **Step 14: Commit**

```bash
git add patches nix/patched.nix flake.nix flake.lock
git commit -m "Vendor external volume patches and add Monitor support"
```

---

### Task 10: NixOS module

**Files:**
- Create: `nix/module.nix`
- Modify: `flake.nix` (add `flake.nixosModules.default` and `checks.nixos-module`)

**Interfaces:**
- Consumes: `self.lib.{pipewirePatches,wireplumberPatches}`, `self.packages.<system>.denon-volume`, `nix/patched.nix`.
- Produces: `services.denon-volume.{enable, package, uid, alsaDevice, route, pcInput, capMv, host, select, logLevel}`.

- [ ] **Step 1: Write `nix/module.nix`**

```nix
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.denon-volume;
  socket = "denon-volume/org.pipewire.ExternalVolume";
  patched = import ./patched.nix {
    inherit pkgs;
    inherit (self.lib) pipewirePatches wireplumberPatches;
  };
in
{
  options.services.denon-volume = {
    enable = lib.mkEnableOption "Denon AVR volume control for a PipeWire HDMI output";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.denon-volume;
      defaultText = lib.literalExpression "denon-pipewire-audio-control.packages.\${system}.denon-volume";
      description = "The denon-volume daemon.";
    };

    uid = lib.mkOption {
      type = lib.types.ints.unsigned;
      example = 1000;
      description = ''
        UID of the user whose session runs the daemon. WirePlumber's configuration is
        system-wide, so the per-user socket path has to be spelled out.
      '';
    };

    alsaDevice = lib.mkOption {
      type = lib.types.str;
      example = "alsa_card.pci-0000_01_00.1";
      description = "PipeWire device name of the card wired to the receiver (see `pw-cli ls Device`).";
    };

    route = lib.mkOption {
      type = lib.types.str;
      default = "hdmi-output-0";
      description = "Route on that card whose volume the receiver controls.";
    };

    pcInput = lib.mkOption {
      type = lib.types.str;
      example = "BD";
      description = "Receiver input the PC is plugged into, as the receiver names it.";
    };

    capMv = lib.mkOption {
      type = lib.types.numbers.between 0 98;
      default = 70;
      description = "Receiver level (MV) that 100% maps to, in steps of 0.5. MV 80 is 0 dB.";
    };

    host = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Receiver address. When null the receiver is found over mDNS.";
    };

    select = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "HTAVR";
      description = "HEOS device ID or friendly name to pick when discovering.";
    };

    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "info";
      description = "`RUST_LOG` filter for the daemon.";
    };
  };

  config = lib.mkIf cfg.enable {
    services.pipewire.package = patched.pipewire;
    services.pipewire.wireplumber.package = patched.wireplumber;

    services.pipewire.wireplumber.extraConfig."90-denon-volume" = {
      "context.properties"."support.varlink" = true;
      "wireplumber.settings"."monitor.alsa.enable-external-volume-control" = true;
      "monitor.alsa.rules" = [
        {
          matches = [ { "device.name" = cfg.alsaDevice; } ];
          actions.update-props."api.alsa.external-volume-control" =
            "unix:/run/user/${toString cfg.uid}/${socket}";
        }
      ];
    };

    systemd.user.sockets.denon-volume = {
      description = "Denon AVR volume control socket";
      wantedBy = [ "sockets.target" ];
      socketConfig.ListenStream = "%t/${socket}";
    };

    systemd.user.services.denon-volume = {
      description = "Denon AVR volume control for PipeWire";
      # Started at login, not only on first connection, so the receiver's state is already
      # known when WirePlumber first asks for it.
      wantedBy = [ "default.target" ];
      requires = [ "denon-volume.socket" ];
      after = [ "denon-volume.socket" ];
      environment.RUST_LOG = cfg.logLevel;
      serviceConfig = {
        Type = "exec";
        Restart = "on-failure";
        ExecStart = lib.escapeShellArgs (
          [
            (lib.getExe' cfg.package "denon-volume")
            "--device"
            cfg.alsaDevice
            "--route"
            cfg.route
            "--pc-input"
            cfg.pcInput
            "--cap-mv"
            (toString cfg.capMv)
          ]
          ++ lib.optionals (cfg.host != null) [
            "--host"
            cfg.host
          ]
          ++ lib.optionals (cfg.select != null) [
            "--select"
            cfg.select
          ]
        );
      };
    };
  };
}
```

- [ ] **Step 2: Export the module and add an evaluation check** in `flake.nix`

Add to the top-level `flake` attribute: `nixosModules.default = import ./nix/module.nix inputs.self;`

Add to `perSystem`'s `checks`:
```nix
            nixos-module =
              let
                eval = inputs.nixpkgs.lib.nixosSystem {
                  modules = [
                    inputs.self.nixosModules.default
                    {
                      nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
                      boot.isContainer = true;
                      system.stateVersion = "26.05";
                      services.pipewire.enable = true;
                      services.denon-volume = {
                        enable = true;
                        uid = 1000;
                        alsaDevice = "alsa_card.pci-0000_01_00.1";
                        pcInput = "BD";
                      };
                    }
                  ];
                };
              in
              pkgs.writeText "denon-volume.service"
                eval.config.systemd.user.units."denon-volume.service".text;
```

This forces the whole module to evaluate but builds only the daemon and a text file, not the patched PipeWire.

- [ ] **Step 3: Run the checks**

Run: `git add -A && nix flake check -L`
Expected: all checks pass, including `nixos-module`. Inspect the unit:
`cat $(nix build .#checks.x86_64-linux.nixos-module --print-out-paths)`. It should contain `ExecStart=… --device alsa_card.pci-0000_01_00.1 --route hdmi-output-0 --pc-input BD --cap-mv 70` and `Environment="RUST_LOG=info"`.

- [ ] **Step 4: Commit**

```bash
git add nix/module.nix flake.nix
git commit -m "Add services.denon-volume NixOS module"
```

---

### Task 11: Spec sync, then deploy and verify on the real system

**Files:**
- Modify: `docs/superpowers/specs/2026-10-11-denon-external-volume-design.md`
- External: `~/Development/nix-config` (the user's system flake). **Confirm with the user before editing or rebuilding it**, because it changes the running system.

- [ ] **Step 1: Bring the spec in line with the decisions above**

Edit the spec:
- **Repository layout:** add `protocol.rs`, `types.rs`, `nix/patched.nix` and `tests/service.rs`.
- **Power:** use `ZM` (`ZM?`, `ZMON`/`ZMOFF`, plus `PWSTANDBY`) instead of `PW`.
- **Patch section:** split into 0019 (the five fixes) and 0020 (Monitor plus reconnect, updates applied through `update_ext_volume`/`update_ext_mute`, and `hardware_volume` recorded on write).
- **Service table:** `GetCapabilities` waits ≤1.5 s for the first state; reads return `OperationUnsupported` until the state is known; Monitor emits only once the level is known; writes don't notify until the receiver confirms; the requested-fraction echo.
- **Discovery:** drop the multiple-receiver warning.
- **Testing:** in-process zlink proxy tests, with spacing checked by review.
- **Module table:** add `uid`; drop `device.routes.default-sink-volume`.

Commit: `git commit -am "Sync design spec with implementation decisions"`

- [ ] **Step 2: Ask the user to confirm deployment**, then add the flake to nix-config

In `~/Development/nix-config`, add an input:
```nix
denon-pipewire-audio-control = {
  url = "git+file:///home/awilliams/Development/denon-pipewire-audio-control";
  inputs.nixpkgs.follows = "nixpkgs";
};
```
In the desktop host's modules, add `inputs.denon-pipewire-audio-control.nixosModules.default` and:
```nix
services.denon-volume = {
  enable = true;
  uid = 1000;
  alsaDevice = "alsa_card.pci-0000_01_00.1";
  pcInput = "BD";
};
```
Follow nix-config's own conventions for where host modules and inputs live; read its layout first. Confirm the UID with `id -u`.

- [ ] **Step 3: Rebuild** with the method nix-config uses (e.g. `nh os switch` or `nixos-rebuild switch --flake …`), then log out and back in, or run `systemctl --user daemon-reload && systemctl --user restart denon-volume.socket denon-volume.service wireplumber pipewire`.

- [ ] **Step 4: Verify end to end**

1. `systemctl --user status denon-volume` → active; the journal shows `connected to receiver`.
2. `journalctl --user -u wireplumber -b | grep -i 'external volume'` → shows the connection; no `requires support.varlink` error.
3. `wpctl get-volume @DEFAULT_AUDIO_SINK@` → matches the receiver (MV62 ≈ 0.74 at a 70 cap).
4. **Ask the user** to turn the receiver knob → `watch -n0.5 wpctl get-volume @DEFAULT_AUDIO_SINK@` follows within about a second.
5. **Ask the user** to drag the slider in pavucontrol → the receiver's display tracks it; releasing leaves the receiver at the slider's position. Dragging to 100% stops at MV70.
6. Mute from pavucontrol → the receiver shows MUTE; unmuting on the remote → pavucontrol updates.
7. **Ask the user** to switch the receiver to another input and move the slider → the receiver doesn't change and the slider snaps back. Switch back to BD.
8. `systemctl --user restart denon-volume` → within about 2 s the knob → slider sync works again (reconnect timer).

Record any failure with the journal excerpt. Don't paper over it.

- [ ] **Step 5: Commit nix-config changes** in that repo, only if the user asks for it.

## Future work (not in this plan)

- Auto-wake and input switching: when the PC starts playing to the HDMI sink, send `ZMON` and `SI<pc_input>`.
