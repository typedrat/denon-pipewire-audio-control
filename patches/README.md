# Patches

`pipewire/0001`–`0018` and `wireplumber/0001` are Valve/Collabora's external volume control
series, taken unmodified from SteamOS source packages (`jupiter-main/pipewire-1:1.6.8-1.5`,
`jupiter-main/wireplumber-0.5.18-1.1`) and rebased onto PipeWire 1.6.9. They are not upstream.

`pipewire/0019` fixes reply parsing and write handling in that series, and `pipewire/0020`
makes ACP follow volume changes pushed by the service through `Monitor`.
