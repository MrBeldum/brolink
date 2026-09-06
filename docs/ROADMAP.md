# Roadmap

## v1.0 – v1.2 (retired)

A custom H.264 protocol, encoder, decoder, tickets, UPnP, STUN, relay,
rendezvous and PIN dialogs. Replaced in 2.0; the git history has it.

## v2.0 (retired)

Glue around Sunshine, Moonlight and Tailscale: the Mac app installed
Moonlight and launched it; BroLink itself did wake, pairing and power.

## v3.0

- [x] The GameStream client compiled into the Mac app (moonlight-common-c,
      VideoToolbox, Opus); no Moonlight to install
- [x] Pairing, app list, launch and resume against Sunshine's HTTPS API with
      a pinned certificate
- [x] A stream window with a toolbar: capture, ⌘ mapping, Keys menu, stats,
      full screen, power, disconnect
- [x] Sunshine's installer shipped in the Windows zip; setup installs it
      offline
- [x] Wake: Fast Startup off, driver keywords for sleep/standby/shutdown,
      a wake-packet listener on the host and **Test wake** on the Mac
- [x] Both Sunshine PIN APIs (with and without pairing ids)
- [x] GPL-3.0-or-later

## Later

- [ ] AV1 decode on M3 and newer (moonlight-common-c negotiates it; the
      VideoToolbox path is HEVC and H.264 today)
- [ ] Gamepads on the Mac (GameController framework to Sunshine's virtual pad)
- [ ] Clipboard sync between Mac and PC
- [ ] Signed Windows and macOS binaries (Authenticode, Developer ID and
      notarization)
- [ ] Menu-bar presence on the Mac, tray icon on Windows
- [ ] Per-PC stream settings
