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
          patchesIn = dir: map (name: dir + "/${name}") (builtins.attrNames (builtins.readDir dir));
        in
        {
          pipewirePatches = patchesIn ./patches/pipewire;
          wireplumberPatches = patchesIn ./patches/wireplumber;
        };

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
