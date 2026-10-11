# Patches

`pipewire/0001`–`0018` and `wireplumber/0001` are Valve/Collabora's external volume control
series, taken unmodified from SteamOS source packages (`jupiter-main/pipewire-1:1.6.8-1.5`,
`jupiter-main/wireplumber-0.5.18-1.1`) and rebased onto PipeWire 1.6.9. They are not upstream.

`pipewire/0019` fixes that series' reply parsing and write handling, along with its varlink
client's message handling, connection lifetime and sync-call timeouts. `pipewire/0020` makes ACP
follow volume changes pushed by the service through `Monitor`, and reconnect when the service
is absent at startup or goes away.

Like PipeWire and WirePlumber themselves, everything under `patches/` is
licensed under the MIT license; see each project's COPYING file. The rest of
this repository is available under either the MIT license or the Apache
License 2.0, at your option.
