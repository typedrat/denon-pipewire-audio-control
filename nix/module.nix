self:
{
  config,
  lib,
  pkgs,
  utils,
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
    services.pipewire.package = lib.mkDefault patched.pipewire;
    services.pipewire.wireplumber.package = lib.mkDefault patched.wireplumber;

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
        RestartSec = 5;
        ExecStart = utils.escapeSystemdExecArgs (
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
