# Roadmap

BroLink is a remote-play product. The transport, pairing, and identity stay
the same; each release hardens that path rather than bolting on unrelated
jobs (backup, offloaded compile, and so on are out of scope).

## v1.0 — Remote Play (this release)

- [x] Game-quality H.264 desktop stream
- [x] Keyboard, relative/absolute mouse, gamepad
- [x] System audio
- [x] PIN pairing + allow-list, with revoke in the host UI
- [x] LAN discovery + STUN tickets + Tailscale detection
- [x] Automatic UPnP / NAT-PMP port mapping
- [x] IPv6 candidates in tickets
- [x] Coordinated hole punch (client advertises its STUN address)
- [x] Optional UDP relay, advertised in the ticket
- [x] Host identity verified against the ticket on connect
- [x] Held keys/buttons released on uncapture and disconnect
- [x] Shared clipboard (short text)
- [x] Adaptive bitrate from loss / RTT
- [x] Auto FFmpeg download on the host
- [x] Saved PCs + auto-reconnect on the client
- [x] Native GUIs aimed at non-developers

## Later

- [ ] HEVC / AV1 encode when the client can decode it
- [ ] VideoToolbox decoder on Apple Silicon
- [ ] Virtual display (headless / exclusive-fullscreen games)
- [ ] Drag-and-drop files
- [ ] Signed Windows/macOS binaries (Developer ID + Authenticode)
- [ ] Bigger clipboard + image paste
- [ ] A hosted anycast relay for people who do not want to run a VPS
