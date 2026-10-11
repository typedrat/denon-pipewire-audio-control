# denon-pipewire-audio-control

Use PipeWire's volume slider as the volume control for a Denon AV receiver.

When a PC feeds a receiver over HDMI, its volume is usually pinned at 100% and
the real volume lives on the receiver's remote or knob. This project makes the
PC's HDMI sink volume *be* the receiver's master volume:

- Moving the slider in pavucontrol, `wpctl`, your bar or with volume keys sets
  the receiver's volume and mute.
- Changes made on the receiver (remote, front knob, HEOS app) show up in
  PipeWire right away.
- The PC only controls the receiver while it is on and switched to the PC's
  input, so a game or app can't change what you're watching on another input.
- A cap stops the PC from ever setting the receiver louder than a level you
  choose.

It is built on `org.pipewire.ExternalVolume`, an out-of-tree PipeWire mechanism
from Valve and Collabora (used on SteamOS for HDMI-CEC volume) that lets an
external service own an ALSA device's volume. This repository contains:

- **`denon-volume`**, a small Rust daemon that serves that interface and talks
  to the receiver over its network control protocol;
- **PipeWire and WirePlumber patches**: Valve's series plus two of ours that fix
  it and add live updates from the receiver;
- **a NixOS module** that builds the patched PipeWire and WirePlumber and runs the
  daemon.

Tested with a Denon AVR-S740H. Other networked Denon and Marantz receivers speak
the same protocol and should work.

## How it works

```
 pavucontrol / wpctl / volume keys
              │
       PipeWire + WirePlumber          (patched ALSA "ACP" code)
              │  Varlink over /run/denon-volume/org.pipewire.ExternalVolume
        denon-volume daemon
              │  telnet, TCP port 23
        Denon receiver
```

The daemon finds the receiver over mDNS (`_heos-audio._tcp`), keeps one telnet
session open, and mirrors its power, input, volume and mute. PipeWire asks the
daemon for the volume, sends it writes, and holds a streaming `Monitor` call
open so that changes made on the receiver arrive immediately.

### Volume scale

The slider percentage is the number on the receiver's display: 62% is MV62,
60.5% is MV60.5. The receiver's scale is already linear in dB (MV80 is 0 dB,
in half-dB steps), so no curve is applied. Writes are rounded to the nearest
half step and never go above the cap (MV70, −10 dB, by default). Ask for more
and the receiver stops at the cap and the slider settles back to it. A level
above the cap set on the receiver itself is shown as it is.

Because 1% is 1 dB, the step size of whatever changes the volume decides how
coarse it feels. For example, `wpctl set-volume @DEFAULT_AUDIO_SINK@ 1%+` is a
1 dB step and `0.5%+` is the receiver's finest step.

### When the PC doesn't get its way

- **Receiver off, or on another input:** writes are ignored and PipeWire snaps
  back to the receiver's real level.
- **Startup, receiver power-on, and switching to the PC's input:** for 3 s,
  writes are ignored the same way. WirePlumber restores a saved route volume at
  these moments, and the receiver's own level should win over a stale one.

## Requirements

- PipeWire **1.6** and WirePlumber **0.5**. The patches are rebased onto PipeWire
  1.6.9 and WirePlumber 0.5.18. The Nix build refuses other major.minor
  versions.
- A Denon or Marantz receiver with network control enabled, reachable on TCP
  port 23. The receiver accepts one telnet client at a time, so stop anything
  else that holds that connection (for example a Home Assistant integration).
- The PC's audio output wired to the receiver over HDMI, and the PipeWire name
  of that card (`pw-cli ls Device`, e.g. `alsa_card.pci-0000_01_00.1`).

## NixOS

Add the flake and enable the module:

```nix
{
  inputs.denon-pipewire-audio-control = {
    url = "github:typedrat/denon-pipewire-audio-control";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, denon-pipewire-audio-control, ... }: {
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      modules = [
        denon-pipewire-audio-control.nixosModules.default
        {
          services.denon-volume = {
            enable = true;
            alsaDevice = "alsa_card.pci-0000_01_00.1";
            pcInput = "BD";
          };
        }
      ];
    };
  };
}
```

After switching, restart the audio stack once with
`systemctl --user restart pipewire wireplumber`.

| Option | Default | |
|---|---|---|
| `enable` | `false` | |
| `alsaDevice` | (required) | PipeWire device name of the card wired to the receiver. |
| `pcInput` | (required) | Receiver input the PC is plugged into, as the receiver names it (`BD`, `GAME`, `SAT/CBL`, …). Query it with `SI?` over telnet, or read it off the receiver's display. |
| `route` | `"hdmi-output-0"` | Route on that card whose volume the receiver controls. |
| `capMv` | `70` | Highest level the PC may set, in steps of 0.5. MV80 is 0 dB. |
| `host` | `null` | Receiver address. When `null`, the receiver is found over mDNS. |
| `select` | `null` | HEOS device ID or friendly name to pick when several receivers answer. |
| `logLevel` | `"info"` | `RUST_LOG` filter for the daemon. |
| `package` | this flake's | The daemon. |

The module:

- replaces `services.pipewire.package` and
  `services.pipewire.wireplumber.package` with patched builds (at `mkDefault`
  priority, so your own override wins);
- adds the WirePlumber configuration that enables Varlink support and points
  `alsaDevice` at the daemon's socket;
- runs the daemon as a socket-activated system service
  (`denon-volume.socket` / `denon-volume.service`), started at boot, with
  `DynamicUser` and systemd sandboxing. There is one daemon for the whole
  machine, since the receiver only accepts one control connection. The socket
  is world-connectable (mode `0666`), because WirePlumber runs as whoever is
  logged in. That grants nothing the receiver's unauthenticated network control
  port doesn't already.

The flake also provides `packages.<system>.pipewire` and `.wireplumber` (the
patched builds alone) and `lib.pipewirePatches` / `lib.wireplumberPatches`.

## Other distributions

Nothing here is NixOS-specific except the module:

1. Apply `patches/pipewire/*.patch` in order to PipeWire 1.6.x and
   `patches/wireplumber/*.patch` to WirePlumber 0.5.x. Build WirePlumber against
   the patched PipeWire.
2. Add a WirePlumber configuration fragment, e.g.
   `~/.config/wireplumber/wireplumber.conf.d/90-denon-volume.conf`:

   ```
   context.properties = {
     support.varlink = true
   }
   wireplumber.settings = {
     monitor.alsa.enable-external-volume-control = true
   }
   monitor.alsa.rules = [
     {
       matches = [ { device.name = "alsa_card.pci-0000_01_00.1" } ]
       actions = {
         update-props = {
           api.alsa.external-volume-control = "unix:/run/denon-volume/org.pipewire.ExternalVolume"
         }
       }
     }
   ]
   ```
3. Build the daemon with `cargo build --release` and run it, ideally
   socket-activated from a systemd socket that listens on the same path:

   ```
   denon-volume --device alsa_card.pci-0000_01_00.1 --pc-input BD
   ```

### Daemon options

```
Usage: denon-volume [OPTIONS] --device <DEVICE> --pc-input <PC_INPUT>

      --device <DEVICE>      PipeWire device name of the card wired to the receiver
      --route <ROUTE>        Route on that card whose volume the receiver controls [default: hdmi-output-0]
      --pc-input <PC_INPUT>  Receiver input the PC is plugged into, as the receiver names it (e.g. BD)
      --cap-mv <CAP_MV>      Highest receiver level (MV) the PC may set; MV 80 is 0 dB [default: 70]
      --host <HOST>          Receiver address; skips mDNS discovery
      --select <SELECT>      HEOS device ID or friendly name to pick when discovering
      --socket <SOCKET>      Socket to listen on when not socket-activated
```

Without socket activation or `--socket`, it listens on
`$XDG_RUNTIME_DIR/denon-volume/org.pipewire.ExternalVolume`. Logging is
controlled by `RUST_LOG`.

## Troubleshooting

- **Daemon log:** `journalctl -u denon-volume -b`. Expect
  `found receiver …` followed by `connected to receiver`.
- **WirePlumber log:** `journalctl --user -u wireplumber -b | grep spa.alsa`.
- **Ask the daemon directly** (`varlinkctl` ships with systemd):

  ```
  S=unix:/run/denon-volume/org.pipewire.ExternalVolume
  varlinkctl introspect $S org.pipewire.ExternalVolume
  varlinkctl call $S org.pipewire.ExternalVolume.ReadVolume '{"device":"","route":"hdmi-output-0"}'
  varlinkctl call --more $S org.pipewire.ExternalVolume.Monitor '{"device":""}'
  ```
- **Discovery fails:** set `host` (or `--host`) to the receiver's address. The
  daemon needs to receive mDNS replies on UDP 5353.
- **The slider doesn't move the receiver:** check that the receiver is on
  `pcInput` (inputs are named as the receiver reports them after `SI`), and that
  nothing else is connected to its port 23.

## Development

```
nix develop        # or direnv: the repository has an .envrc
cargo test
nix flake check    # build, clippy, tests, formatting and a NixOS module check
```

The design is documented in `docs/superpowers/specs/`.

## Credits

The external volume mechanism and patches `0001`–`0018` (PipeWire) and the
WirePlumber patch are by Arun Raghavan (Valve), Wim Taymans and Julian Bouzas
(Collabora), taken from SteamOS source packages. See `patches/README.md`.

## License

Everything under `patches/` is MIT, like PipeWire and WirePlumber. Everything
else is available under either the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option.
