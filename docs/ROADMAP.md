# Roadmap

BroLink 2.0 stopped being a streaming stack and became the glue around
Sunshine, Moonlight and Tailscale. Everything the stream itself needs is
their job; what stays here is turning the PC on and off, pairing without
touching it, and the one-click path.

## v1.0 – v1.2 — the custom stack (retired)

Own H.264 protocol, encoder, decoder, tickets, UPnP, STUN, relay,
rendezvous, PIN dialogs. Replaced wholesale in 2.0; the git history has it.

## v2.0 — Sunshine + Moonlight + Tailscale

- [x] Windows host: background control service on the tailnet, identity by
      `tailscale whois`
- [x] One-click setup: silent Sunshine install, `--creds`, firewall,
      Wake-on-LAN
- [x] Mac: PCs listed from Tailscale, wake → pair → Moonlight in one click
- [x] Pairing without the PC's screen (PIN forwarded to Sunshine's API)
- [x] Sleep / restart / shut down from the Mac, refusable on the host
- [x] Moonlight installed from the Mac app
- [x] Apollo used automatically when installed (virtual display)
- [x] Both GUIs on the shared theme, every screen renderable to PNG

## Later

- [ ] Signed Windows/macOS binaries (Authenticode, Developer ID + notarize)
- [ ] A wake helper mode for a second always-on Windows box on the LAN
- [ ] Menu-bar presence on the Mac, tray icon on Windows
- [ ] Per-PC stream settings
- [ ] Windows as a client too (Moonlight's CLI is the same)
