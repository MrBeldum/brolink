# Contributing

Remote Play is the product. Keep changes to the wire protocol
backward-compatible or bump `PROTO_VERSION`. Ticket version 2 (IPv4) must
keep decoding; version 3 is for IPv6.

Before opening a PR:

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets    # must be warning-free
cargo test --workspace
.\scripts\test-loopback.ps1               # the end-to-end check (video + audio)
```

The loopback script is the one that catches real breakage: it runs a host and
a client against the real GPU encoder and requires 30 decoded frames over both
a bare address and a ticket. Unit tests alone have never caught a capture or
encoder-flag regression.

Changes to encoder flags deserve particular care. Every encoder is configured
for a single slice per picture, because the frame splitter treats a complete
slice NAL as a complete frame; a multi-slice stream would tear. `libx264`
needs `-x264-params sliced-threads=0:slices=1` for this, and `gdigrab` capture
needs an explicit `-pix_fmt nv12` or it produces 4:4:4 output the client
cannot decode.

Please do not wrap Sunshine or Moonlight as a hidden subprocess — BroLink
owns its protocol.
