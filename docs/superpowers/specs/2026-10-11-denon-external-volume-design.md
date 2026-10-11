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
3. **Live sync: patch ACP to call `Monitor()`.** Valve's client never calls it.
4. **Input gating.** Writes take effect only while the receiver is `PWON` and
   `SI` equals the configured PC input. Otherwise the write is dropped and the
   real state is re-published so the slider snaps back.
5. **Discovery: mDNS** on `_heos-audio._tcp`. A selector picks one receiver by
   `did` or friendly name, and `host` overrides discovery entirely.
6. **Implementation: Rust 2024, tokio, `zlink` 0.4, `mdns-sd`.** This follows
   the Valve reference server.

## Repository layout

```
denon-pipewire-audio-control/
├── flake.nix
├── Cargo.toml                 # virtual workspace, resolver = "3"
├── crates/denon-volume/
│   └── src/
│       ├── main.rs            # args, socket activation, signals
│       ├── service.rs         # org.pipewire.ExternalVolume (zlink)
│       ├── avr.rs             # telnet session, parse, send queue
│       ├── discovery.rs       # mDNS browse/resolve
│       ├── state.rs           # cached receiver state + watch channel
│       └── scale.rs           # fraction ↔ MV mapping (pure)
├── patches/pipewire/          # Valve 0001–0018 + 0019 (Monitor support)
├── patches/wireplumber/       # Valve 0001
└── nix/module.nix
```

## Components

### `scale`

Pure functions `to_mv(v, cap) -> Mv` (`round(v·200)` half steps, clamped to
`cap`) and `from_mv(mv) -> f64` (half steps ÷ 200). `Mv` holds
half-steps as an integer (MV62 = 124, MV60.5 = 121) and formats as the wire
form (`62`, `605`, `00`). No I/O.

### `avr`

Owns the one telnet connection.

- **Connect:** resolve the target through `discovery` (or `host`), then connect
  to port 23. On connect, send `PW?`, `SI?`, `MV?`, `MU?`.
- **Parse:** split on `\r` and recognise `PW(ON|STANDBY)`, `SI<input>`,
  `MV<nn[5]>`, `MU(ON|OFF)`. Ignore `MVMAX …` and anything unknown.
- **Send:** a queue that coalesces, so only the latest pending `MV` and the
  latest pending `MU` are kept, with at least 50 ms between commands.
- **Reconnect:** on drop or error, back off exponentially from 1 s to 30 s. A
  failed connect re-runs discovery before the next attempt.

### `discovery`

Browse `_heos-audio._tcp.local.` with `mdns-sd`. Match the configured selector
against TXT `did` or the instance/host name. With no selector, take the first
answer and log a warning if more than one receiver answers. Return an IPv4
address.

### `state`

`struct AvrState { power: Option<bool>, input: Option<String>, mv: Option<Mv>, mute: Option<bool>, connected: bool }`
published through `tokio::sync::watch`. `active()` is true when the receiver is
connected, `power == Some(true)`, and `input == pc_input`.

### `service`

Serves `org.pipewire.ExternalVolume` for the configured `device` and `route`.

| Method | Behaviour |
|---|---|
| `GetCapabilities` | Always succeeds for the configured device. Advertises `readVolume`, `writeVolumeAbsolute`, `readMute`, `writeMuteValue` and `writeMuteToggle`; `volumeRange {min: 0, max: cap ÷ 100, step: 0.005}` (ACP ignores it); `routes: [route]`. |
| `ReadVolume` / `ReadMute` | Answer from the cache. Before the first state is known, return `[0.0]` / `false`. |
| `WriteVolumeAbsolute` | Reply immediately. If `active()`, queue `MV<to_mv(max(volume))>`; otherwise re-publish the state so it reaches Monitor subscribers. |
| `WriteVolumeRelative` | Reply `OperationUnsupported` (not advertised). |
| `WriteMuteValue` / `WriteMuteToggle` | Reply immediately; gated the same way as volume writes. |
| `Monitor` | Streaming reply (`more`). Sends the current state first, then one reply per change in `mv` or `mute`, or on re-publish. |

A device mismatch returns `InvalidDevice` and a route mismatch returns
`InvalidRoute`.

### `main`

Uses the systemd `LISTEN_FDS` listener, falling back to binding
`$XDG_RUNTIME_DIR/denon-volume/org.pipewire.ExternalVolume` as the reference
server does. SIGINT, SIGTERM and SIGHUP cause a clean exit. Logging goes
through `tracing` (`RUST_LOG`).

CLI flags: `--device`, `--route`, `--pc-input`, `--cap-mv` (default `70`),
`--host`, `--select`, `--socket`.

## PipeWire patch 0019 — ACP Monitor support

The varlink client refuses sync calls while a `more` call is pending on the
same connection. `Monitor` therefore runs on a dedicated connection.

- Add `monitor_client` and its listener to `struct spa_acp_ext_volume`.
- In `spa_acp_ext_volume_init`, after capabilities: if `READ_VOLUME` or
  `READ_MUTE` is set, connect `monitor_client` and call `Monitor(device)` with
  `more=true` and a reply callback.
- The callback parses `route`, `volume[]` and `mute?`, then calls back into the
  ACP card through a new card-level callback registered by `acp.c`. That
  callback finds the active port; if `route` matches, it updates the stored
  port volume and mute and emits the same change notification as the hardware
  mixer path. It must not go through the `Write*` path, so that an update is
  never echoed back to the daemon.
- `client_disconnect` (currently a TODO) schedules a 1 s reconnect timer that
  re-creates both clients and re-issues `Monitor`. While disconnected, writes
  fail and are logged, as they do now.
- `spa_acp_ext_volume_destroy` also tears down `monitor_client` and the timer.

## NixOS module (`services.denon-volume`)

| Option | Default |
|---|---|
| `enable` | `false` |
| `package` | the flake's daemon |
| `alsaDevice` | — (e.g. `"alsa_card.pci-0000_01_00.1"`) |
| `route` | `"hdmi-output-0"` |
| `pcInput` | — (e.g. `"BD"`) |
| `capMv` | `70` |
| `host` | `null` (use discovery) |
| `select` | `null` (`did` or friendly name) |
| `logLevel` | `"info"` |

When enabled, the module:

- sets `services.pipewire.package` and `services.pipewire.wireplumber.package`
  to builds with the patches applied, with WirePlumber built against the
  patched PipeWire. No global overlay is used.
- adds the WirePlumber config: `context.properties."support.varlink" = true`,
  `wireplumber.settings."monitor.alsa.enable-external-volume-control" = true`,
  and a `monitor.alsa.rules` entry for `alsaDevice` that sets
  `api.alsa.external-volume-control = "unix:/run/user/<uid>/denon-volume/org.pipewire.ExternalVolume"`
  (built from `%t` with the user's runtime directory) and
  `device.routes.default-sink-volume = 1.0`.
- defines a systemd user socket (`ListenStream=%t/denon-volume/org.pipewire.ExternalVolume`)
  and a service (`Type=exec`, with `ExecStart` built from the options).

The flake also exposes `packages.pipewire` and `packages.wireplumber` so the
patched builds can be checked alone.

## Testing

- **`scale`:** unit tests for the round trip (every level up to the cap maps
  back to itself), 0.62 ↔ MV62, 0.613 → MV61.5, 0.9 → MV70 (the cap), 0 → MV0,
  MV80 reading as 0.80, and the wire formatting `605` / `00`.
- **`avr`:** tokio tests against an in-process fake Denon TCP server. They
  cover partial-line buffering, half-step parsing, ignoring `MVMAX`,
  coalescing, the 50 ms spacing, and reconnect after the server drops.
- **`service`:** an integration test that runs the daemon on a temporary
  socket against the fake server and drives it with `varlinkctl`. It covers
  gated and ungated writes, the Monitor first reply, and Monitor on a remote
  change.
- **Patch 0019:** `nix build .#pipewire`, then a manual live check: turn the
  receiver knob and watch `wpctl get-volume @DEFAULT_AUDIO_SINK@`; move the
  slider and watch the receiver's display.

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
- Upstreaming patch 0019.
