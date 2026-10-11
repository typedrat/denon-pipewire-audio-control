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
