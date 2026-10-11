# Denon AVR volume control for PipeWire — design

**Date:** 2026-10-11
**Status:** Approved for planning

## Goal

Make the PipeWire volume slider for the PC's HDMI output drive the main-zone
volume and mute of a Denon AVR-S740H, and keep the slider in sync with changes
made on the receiver (remote, front knob, HEOS app).

This is done through Valve/Collabora's `org.pipewire.ExternalVolume` Varlink
mechanism, which is not upstream. The source material is in
`~/Downloads/pipewire-external-volume.tar.gz`: PipeWire patches 0001–0018
(rebased onto 1.6.9), one WirePlumber 0.5.18 patch, the interface IDL, and
Valve's `cec-audio-control` reference server.

## Environment facts

| | |
|---|---|
| Receiver | Denon AVR-S740H, friendly name `HTAVR`, firmware 3.139.173 |
| Control protocol | Telnet (TCP 23). Commands and events are `\r`-terminated lines. |
| Observed state | `PWON`, `MV62`, `MVMAX 98`, `MUOFF`, `SIBD` |
| mDNS | `_heos-audio._tcp` on `HTAVR.local`, TXT `model=Denon AVR-S740H`, `did=3C424E4F0005CDFEC712` |
| SSDP | No replies (probably dropped by the host firewall); not used |
| PipeWire on host | 1.6.9 (matches the patch rebase target) |
| HDMI card | `alsa_card.pci-0000_01_00.1` (GB202), route `hdmi-output-*` (confirm with `pw-cli`) |

## Decisions

1. **Protocol: telnet.** It supports absolute volume (`MVnn` / `MVnn5` for
   0.5 dB steps) and explicit mute (`MUON`/`MUOFF`), and it pushes every state
   change without polling.
2. **Volume mapping: the display number, capped.** The PipeWire fraction is
   `v = MV / 100`, so the slider percentage reads the same as the receiver's
   display: MV62 is 62% and MV60.5 is 60.5%. `MV` is already linear in dB, and
   PipeWire's software volume stays at 100% for this device, so no cubic curve
   is applied. Writes round to the nearest 0.5 step and clamp to
   `[MV 0, cap]`, and `v ≤ 0` maps to MV 0. The default cap is
   **MV 70 (−10 dB)**: dragging past 70% leaves the receiver at MV70, and the
   slider settles back to 70%. A level above the cap set on the receiver
   itself reads back as it is. Two side effects are accepted: pavucontrol's
   dB label (computed from its cubic curve) won't match the receiver, and a
   5% volume-key step is a 5 dB change.
3. **Live sync: patch ACP to call `Monitor()`.** Valve's client never calls it,
   and its request/reply handling needs fixes first (patches 0019 and 0020).
4. **Input gating.** Writes take effect only while the main zone is on (`ZM`)
   and `SI` equals the configured PC input. Otherwise the write is dropped and the
   real state is re-published so the slider snaps back.
5. **Discovery: mDNS** on `_heos-audio._tcp`. A selector picks one receiver by
   `did` or friendly name, and `host` overrides discovery entirely.
6. **Implementation: Rust 2024, tokio, `zlink` 0.4, `mdns-sd`.** This follows
   the Valve reference server.

## Repository layout

```
denon-pipewire-audio-control/
├── flake.nix                  # flake-parts, crane, numtide/devshell, treefmt-nix
├── .envrc                     # use flake
├── Cargo.toml                 # virtual workspace, resolver = "3"
├── crates/denon-volume/
│   ├── src/
│   │   ├── main.rs            # args, socket activation, signals
│   │   ├── service.rs         # org.pipewire.ExternalVolume (zlink)
│   │   ├── types.rs           # wire types and errors for the interface
│   │   ├── avr.rs             # telnet session, send queue, reconnect
│   │   ├── protocol.rs        # telnet lines: parse, format, framing
│   │   ├── discovery.rs       # mDNS browse/resolve
│   │   ├── state.rs           # cached receiver state, write gating
│   │   └── scale.rs           # fraction ↔ MV mapping (pure)
│   └── tests/service.rs       # service tests over a real Unix socket
├── patches/pipewire/          # Valve 0001–0018, plus 0019 and 0020
├── patches/wireplumber/       # Valve 0001
└── nix/
    ├── module.nix
    └── patched.nix            # PipeWire/WirePlumber with the patches applied
```

The flake builds the daemon with crane and provides the dev shell (devshell)
and formatting (treefmt-nix). Its checks cover build, clippy, tests and the
NixOS module.

## Components

### `scale`

Pure functions `to_mv(v, cap) -> Mv` (`round(v·200)` half steps, clamped to
`cap`) and `from_mv(mv) -> f64` (half steps ÷ 200). `Mv` holds
half-steps as an integer (MV62 = 124, MV60.5 = 121) and formats as the wire
form (`62`, `605`, `00`). No I/O.

### `protocol`

Telnet lines are ASCII, `\r`-terminated in both directions. Messages are held
bare, without the terminator, and it is appended in one place when sending.

- **Receive:** `LineBuffer` reassembles lines from arbitrarily split or merged
  TCP reads. The parser recognises `ZMON`, `ZMOFF` and `PWSTANDBY` (power),
  `SI<input>`, `MV<nn[5]>` and `MU(ON|OFF)`. `MVMAX …`, `PWON` and anything
  unknown are ignored.
- **Power** is tracked with `ZM` (the main zone), not `PW`: `ZM?` on connect,
  `ZMON` as on, `ZMOFF` and `PWSTANDBY` as off.
- **Send:** `MV<nn[5]>`, `MUON`, `MUOFF`.

### `avr`

Owns the one telnet connection.

- **Connect:** resolve the target through `discovery` (or `host`), then connect
  to port 23. On connect, clear the outbox and send `ZM?`, `SI?`, `MV?`, `MU?`.
- **Send:** an outbox that coalesces, so only the latest pending `MV` and the
  latest pending `MU` are kept, with at least 50 ms between commands. A pending
  mute goes before a pending volume. Commands queued while disconnected are
  dropped on connect, because they were decided against state that may no
  longer hold.
- **Heartbeat and dead-peer detection:** the heartbeat (`ZM?`) interval is
  configurable; `main` passes 30 s. A peer that vanishes without a FIN or RST
  would otherwise leave the read pending while writes succeed into the kernel
  buffer, so if no bytes arrive within 2 × the heartbeat interval the session
  ends and reconnects.
- **Reconnect:** on drop, error or idle timeout, back off exponentially from 1 s
  to 30 s, reset by a successful connect. Each attempt re-runs discovery when
  no `host` is set.

### `discovery`

Browse `_heos-audio._tcp.local.` with `mdns-sd`. Match the configured selector
against TXT `did` or the first label of the instance or host name,
case-insensitively. The first matching answer wins; with no selector, that is
the first answer. Return its IPv4 address.

### `state`

`struct AvrState { connected, power: Option<bool>, input: Option<String>, mv: Option<Mv>, mute: Option<bool>, … }`
published through `tokio::sync::watch`. `active()` is true when the receiver is
connected, `power == Some(true)`, and `input == pc_input`. `Shared` wraps the
state and decides which PipeWire requests become receiver commands:

- **Write gating:** a request while not `active()` is refused and the state is
  re-published, so subscribers drop the write and the slider snaps back.
- **Cap:** an over-cap request is clamped to the cap before rounding. NaN and
  negative requests mean silence.
- **Echo suppression:** the requested fraction is remembered with the level it
  rounded to. While the receiver sits at that level the exact fraction is
  reported, so PipeWire sees its write echoed unchanged rather than nudged by
  half-dB rounding. Any other level reports as `MV ÷ 100`.
- **Active writes** don't notify subscribers until the receiver confirms the
  new level. Notifying earlier would report the old level and drag the slider
  back mid-change.

### `service`

Serves `org.pipewire.ExternalVolume` for the configured `device` and `route`.

| Method | Behaviour |
|---|---|
| `GetCapabilities` | Waits up to 1.5 s for the first volume and mute, since PipeWire reads right after and gives that call 2 s; then succeeds for the configured device either way. Advertises `readVolume`, `writeVolumeAbsolute`, `readMute`, `writeMuteValue` and `writeMuteToggle`; `volumeRange {min: 0, max: cap ÷ 100, step: 0.005}` (ACP ignores it); `routes: [route]`. |
| `ReadVolume` / `ReadMute` | Answer from the cache. Until the state is known, return `OperationUnsupported`. |
| `WriteVolumeAbsolute` | Reply immediately. Gated by `state` as above; if the request passes, queue `MV<level>` for the highest channel value. |
| `WriteVolumeRelative` | Reply `OperationUnsupported` (not advertised). |
| `WriteMuteValue` / `WriteMuteToggle` | Reply immediately; gated the same way as volume writes. Toggle inverts the cached mute. |
| `Monitor` | Streaming reply (`more`). Emits the current state once the level is known, then one reply per state change. |

A device mismatch returns `InvalidDevice` and a route mismatch returns
`InvalidRoute`, except on `Monitor`: a streamed reply can't carry an error, so
a `Monitor` for an unknown device only logs a warning and streams anyway. This
is an accepted limitation.

### `main`

Registers the SIGINT, SIGTERM and SIGHUP handlers first; any of them causes a
clean exit. Then it uses the systemd `LISTEN_FDS` listener (only if `LISTEN_PID`
names this process), falling back to binding
`$XDG_RUNTIME_DIR/denon-volume/org.pipewire.ExternalVolume`. A guard removes a
self-bound socket on every exit path. `bind` refuses to replace a socket a live
server answers on, and replaces a stale one. Logging goes through `tracing`
(`RUST_LOG`).

CLI flags: `--device`, `--route`, `--pc-input`, `--cap-mv` (default `70`, in
0.5 steps up to 98), `--host`, `--select` (conflicts with `--host`), `--socket`.

## PipeWire patches

Valve's series is not enough on its own: its client has parsing and lifetime
bugs, and it never calls `Monitor`. Two further patches follow it.

### Patch 0019 — fixes to Valve's series

- `ReadVolume` and `ReadMute` read their value from the right JSON iterator, and
  the varlink client keeps parsing past `continues`, whatever the key order.
- The client delivers every complete message in a read and compacts its buffer,
  so long-lived connections don't overflow it. Reply callbacks may make new
  calls or destroy the client.
- Absolute writes send the new volume, not the previous one. Writes are oneway,
  so a second write while one awaits a reply no longer fails. Volumes reported
  by the service are clamped to the valid range.
- Writes use `MSG_NOSIGNAL` and never leave half a message on the socket: one
  that would block is dropped with `-EAGAIN`, one cut short disconnects. Sync
  calls send with `MSG_DONTWAIT`.
- Once disconnected, calls fail with `-ENOTCONN` instead of writing to a closed
  fd, and replies sent just before a hangup are still read.
- Sync-call timeouts of a second or more are honoured; a call that times out or
  fails disconnects, so a late reply can't be taken as the answer to the next
  call. Connect failures report the real errno at debug level and close the
  socket.
- Destroyed clients are freed, and the service connection is set up once per
  card rather than once per device.

### Patch 0020 — Monitor support

The varlink client refuses sync calls while a `more` call is pending on the
same connection, so `Monitor` runs on a second connection.

- After capabilities, if `READ_VOLUME` or `READ_MUTE` is set, ACP calls
  `Monitor(device)` with `more=true`. Each update goes to the card, which finds
  the active route and, if `route` matches, applies the volume and mute and
  emits the same change notification as the hardware mixer path. It doesn't go
  through the `Write*` path, so an update is never echoed back to the daemon.
- `hardware_volume` is recorded on every write, so an update equal to what was
  just written is recognised as no change.
- If the service is unreachable at startup or goes away later, both connections
  are retried on a timer from 1 s backing off to 30 s, with separate backoff
  for the main connection and for Monitor, so a service that keeps refusing
  Monitor doesn't delay reconnecting the main connection. A replacement
  connection is swapped in only once it works; calls fail meanwhile instead of
  falling through to the card's own mixer. A Monitor stream that ends is
  restarted.
- When the service is absent at startup, capabilities arrive late. A `ready`
  callback then refreshes the devices, installing their volume and mute hooks
  and dropping any software volume applied before.
- Logging is quiet: one warning when the service is lost, debug-level retries,
  and one info message on recovery.

## NixOS module (`services.denon-volume`)

| Option | Default |
|---|---|
| `enable` | `false` |
| `package` | the flake's daemon |
| `uid` | — (required; e.g. `1000`) |
| `alsaDevice` | — (e.g. `"alsa_card.pci-0000_01_00.1"`) |
| `route` | `"hdmi-output-0"` |
| `pcInput` | — (e.g. `"BD"`) |
| `capMv` | `70` |
| `host` | `null` (use discovery) |
| `select` | `null` (`did` or friendly name) |
| `logLevel` | `"info"` |

`uid` names the user whose session runs the daemon. WirePlumber's configuration
is system-wide, so the per-user socket path has to be spelled out.

When enabled, the module:

- sets `services.pipewire.package` and `services.pipewire.wireplumber.package`,
  with `mkDefault`, to builds with the patches applied, with WirePlumber built
  against the patched PipeWire (`nix/patched.nix`). No global overlay is used.
- adds the WirePlumber config: `context.properties."support.varlink" = true`,
  `wireplumber.settings."monitor.alsa.enable-external-volume-control" = true`,
  and a `monitor.alsa.rules` entry for `alsaDevice` that sets
  `api.alsa.external-volume-control = "unix:/run/user/<uid>/denon-volume/org.pipewire.ExternalVolume"`.
- defines a systemd user socket (`ListenStream=%t/denon-volume/org.pipewire.ExternalVolume`)
  and a service (`Type=exec`, `Restart=on-failure`, `RestartSec=5`, started at
  login, with `ExecStart` built from the options using systemd escaping).

The flake also exposes `packages.pipewire` and `packages.wireplumber` so the
patched builds can be checked alone. Its `nixos-module` check evaluates the
module and asserts that the socket path in the WirePlumber config matches the
systemd socket.

## Testing

- **`scale`:** unit tests for the round trip (every level up to the cap maps
  back to itself), 0.62 ↔ MV62, 0.613 → MV61.5, 0.9 → MV70 (the cap), 0 → MV0,
  MV80 reading as 0.80, and the wire formatting `605` / `00`.
- **`protocol`, `state`, `discovery`:** unit tests for line reassembly and
  parsing, write gating, echo suppression, notify-on-confirm, the cap, and
  selector matching.
- **`avr`:** tokio tests against an in-process fake Denon TCP server. They
  cover initial queries, queued commands, coalescing, and reconnect after the
  server drops. A fake receiver that accepts and then goes silent checks
  dead-peer detection. Command spacing is checked by code review.
- **`service`:** tests in `tests/service.rs` run the service in-process on a
  temporary Unix socket and drive it with a zlink proxy. They cover
  capabilities, reads before and after the state is known, gated and ungated
  writes, and Monitor on a remote change. `varlinkctl` is a manual check.
- **Varlink client (patch 0019):** verified with an ASan/UBSan harness kept
  outside the repo.
- **Patches 0019 and 0020:** `nix build .#pipewire`, then a live check: turn
  the receiver knob and watch `wpctl get-volume @DEFAULT_AUDIO_SINK@`; move the
  slider and watch the receiver's display. A read-only smoke test against the
  real receiver returned `ReadVolume` 0.62 at MV62.

## Volume step size

Under this mapping, every 1% of PipeWire volume is 1 dB on the receiver, so the
step size of whatever changes the volume sets how coarse it feels:

- **Waybar** (`wireplumber` module, scroll on the icon): the `scroll-step`
  option, in percent, defaults to 1.0, which is 1 dB per tick. It is set in
  nix-config at `modules/home-manager/desktop/hyprland/bar/waybar/default.nix`.
- **Hyprland volume keys** (none are bound today): bind
  `XF86AudioRaiseVolume` / `XF86AudioLowerVolume` to
  `wpctl set-volume @DEFAULT_AUDIO_SINK@ 1%+` / `1%-`. The step in that command
  is the dB step: `0.5%` gives the receiver's finest step. Use `bindel`-style
  repeat so holding a key keeps going. The bindings live next to the media keys
  in nix-config at `modules/home-manager/desktop/hyprland/default.nix`.
- **pavucontrol / other mixers:** dragging is continuous. Keyboard steps inside
  pavucontrol are fixed by the application.

The daemon enforces the cap whatever the step, so a `wpctl … 5%+` can't go past
it.

## Out of scope / future work

- **Auto-wake and input switching:** when the PC starts playing to the HDMI
  sink, send `PWON` and `SI<pc_input>`. Recorded for a future plan.
- Per-channel balance, zone 2, and other Denon features.
- Upstreaming patches 0019 and 0020.
