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

## v1.1 — Turn it on, turn it off

- [x] Wake-on-LAN from the Mac: on the LAN by broadcast, from anywhere via the router mapping
- [x] Connect wakes a sleeping PC automatically; a Wake button for the rest
- [x] Sleep / restart / shut down from the client, with the host owner able to refuse
- [x] Host reports the adapter's wake settings and enables them (UAC) in one click
- [x] Permanent UPnP / week-long NAT-PMP leases so a sleeping PC stays reachable
- [x] Rendezvous on the relay: lookup by host key, coordinated hole punch, tickets survive IP changes
- [x] IPv6 actually works: the host listens on v6 as well as v4
- [x] Fast reconnect: a returning client replaces its own stale session
- [x] Host quality settings cap the client's request instead of being ignored

## v1.2 — What the Mac said

- [x] Correct colour: the stream is limited-range BT.601 end to end, matching the decoder
- [x] No letterboxing: a non-16:9 desktop streams at its own aspect ratio, and absolute mouse mapping is right with it
- [x] Mouse released whenever the window loses focus; fn+F8 on macOS
- [x] Mac audio opens in the device's native format, with a fallback to its default configuration
- [x] Both GUIs rebuilt on one shared theme (crates/ui)
- [x] Every screen renders to PNG for review without a PC
- [x] Docs: the Gatekeeper prompt, signing, and a relay when the router cannot open a port

## Later

- [ ] HEVC / AV1 encode when the client can decode it
- [ ] VideoToolbox decoder on Apple Silicon
- [ ] Virtual display (headless / exclusive-fullscreen games)
- [ ] Drag-and-drop files
- [ ] Signed Windows/macOS binaries (Developer ID + Authenticode)
- [ ] Bigger clipboard + image paste
- [ ] A hosted anycast relay for people who do not want to run a VPS
- [ ] Wake from a full shutdown over the internet (needs a helper on the LAN or router support)
- [ ] Run as a service so the login screen can be driven remotely
