# Roadmap

ForgeLink is built as one pair of apps. Each function ships as a complete
slice; the transport, pairing, and identity stay the same.

## v0.1 — Remote Play (this release)

- [x] Game-quality H.264 desktop stream
- [x] Keyboard, relative/absolute mouse, gamepad
- [x] System audio
- [x] PIN pairing + allow-list
- [x] LAN discovery + STUN tickets + Tailscale detection (no UPnP/NAT-PMP, and no signalling for true hole punching -- see README)
- [x] Optional UDP relay, advertised in the ticket (routing is unit-tested; not yet exercised over a real network)
- [x] Host identity verified against the ticket on connect
- [x] Held keys/buttons released on uncapture and disconnect

## v0.2 — Remote Play hardening

- [ ] HEVC / AV1 encode when the client can decode it
- [ ] VideoToolbox decoder on Apple Silicon
- [ ] Virtual display (headless / exclusive-fullscreen games)
- [ ] Clipboard, drag-and-drop
- [ ] Bitrate auto-tune from loss/RTT
- [ ] Signed Windows/macOS binaries

## v0.3 — Backup

The Windows PC as a personal backup target the Mac can reach from anywhere
using the same ticket and identity.

## v0.4 — Extra compute

Submit jobs (encodes, builds, inference) to the Windows box and stream
progress back on the control channel.
