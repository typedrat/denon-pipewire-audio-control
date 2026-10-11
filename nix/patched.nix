{
  pkgs,
  lib ? pkgs.lib,
  pipewirePatches,
  wireplumberPatches,
}:
assert lib.assertMsg (lib.versions.majorMinor pkgs.pipewire.version == "1.6")
  "denon-volume's PipeWire patches are rebased onto PipeWire 1.6.9, but pkgs.pipewire is ${pkgs.pipewire.version}";
assert lib.assertMsg (lib.versions.majorMinor pkgs.wireplumber.version == "0.5")
  "denon-volume's WirePlumber patches are rebased onto WirePlumber 0.5.18, but pkgs.wireplumber is ${pkgs.wireplumber.version}";
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
