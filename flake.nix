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

      flake.lib =
        let
          patchesIn =
            dir:
            map (name: dir + "/${name}") (
              builtins.filter (name: builtins.match ".*\\.patch" name != null) (
                builtins.attrNames (builtins.readDir dir)
              )
            );
        in
        {
          pipewirePatches = patchesIn ./patches/pipewire;
          wireplumberPatches = patchesIn ./patches/wireplumber;
        };

      flake.nixosModules.default = import ./nix/module.nix inputs.self;

      perSystem =
        { config, pkgs, ... }:
        let
          patched = import ./nix/patched.nix {
            inherit pkgs;
            inherit (inputs.self.lib) pipewirePatches wireplumberPatches;
          };
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
            inherit (patched) pipewire wireplumber;
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
              let
                inherit (inputs.nixpkgs) lib;
                wp = eval.config.services.pipewire.wireplumber.extraConfig."90-denon-volume";
                # The socket unit's %t is the user's runtime dir, /run/user/<uid>. WirePlumber is
                # configured with that path spelled out, so the two must name the same socket.
                expectedPath = "unix:/run/user/1000/denon-volume/org.pipewire.ExternalVolume";
                rule = builtins.head wp."monitor.alsa.rules";
                wpOk =
                  assert lib.assertMsg (
                    wp."context.properties"."support.varlink" == true
                  ) "support.varlink must be true";
                  assert lib.assertMsg (
                    wp."wireplumber.settings"."monitor.alsa.enable-external-volume-control" == true
                  ) "monitor.alsa.enable-external-volume-control must be true";
                  assert lib.assertMsg (
                    rule.actions.update-props."api.alsa.external-volume-control" == expectedPath
                  ) "api.alsa.external-volume-control must be ${expectedPath}";
                  true;
              in
              assert wpOk;
              pkgs.runCommand "denon-volume-module-check"
                {
                  service = eval.config.systemd.user.units."denon-volume.service".text;
                  socket = eval.config.systemd.user.units."denon-volume.socket".text;
                  passAsFile = [
                    "service"
                    "socket"
                  ];
                }
                ''
                  # systemd quoting wraps every argument, so the flag/value pairs are quoted individually.
                  grep -F '"--pc-input" "BD"' "$servicePath"
                  grep -F '"--cap-mv" "70"' "$servicePath"
                  grep -F 'RUST_LOG=info' "$servicePath"
                  grep -F 'ListenStream=%t/denon-volume/org.pipewire.ExternalVolume' "$socketPath"
                  cp "$servicePath" $out
                '';
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
