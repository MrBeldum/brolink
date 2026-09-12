# Changelog

The release workflow publishes the section that matches the tag as the
GitHub release notes, so each version gets a heading of the form
`## X.Y.Z (date)`.

## Unreleased

- A connected stream that remains black now explains that the PC is sending
  a black picture and needs an active physical or virtual display. Missing
  video, frozen video, and decoder failures have separate persistent notices.
- The notice offers **Restart stream**, which starts a fresh Desktop capture
  instead of resuming the same broken capture. Failure to stop Sunshine's
  previous session is reported instead of silently resuming the wrong app.
- Frame rate and bitrate no longer remain frozen at their last good values
  when video stops. Stream overlays no longer pass clicks through to the PC.
- VideoToolbox callback errors now request a recovery frame and recreate
  failed decoder sessions, just like synchronous decode errors.
- The live video test now checks for a visible picture, with a separate
  mode that verifies the black-picture notice and restart action. Synthetic
  video tests cover the Mac decoder and GPU renderer.

## 3.1.1 (2026-09-09)

A hardening release: both apps parse the network more strictly, update
themselves more carefully, and the stream validates what the PC sends
before acting on it. Nothing changes in how you use BroLink.

### Control API

- Requests and replies are read within a fixed size budget, must use
  proper HTTP line endings and versions, and are refused when they carry
  chunked encoding, duplicate framing headers or a malformed
  `Content-Length`. Bodies grow only as bytes arrive.
- BroLink Host checks who is asking before it reads a single header,
  keeps at most sixteen connections open, and turns away browser
  requests (including ones a rebound hostname makes look local).

### Updates

- The Mac rechecks a cached download against the SHA-256 and size GitHub
  publishes before installing it, stages the new app on the same volume
  so a failed copy cannot leave a half-installed app, and no longer
  passes its own path through a shell when relaunching.
- BroLink Host accepts only a 64-bit Windows executable image, installs
  one update at a time, and puts the previous executable back if the new
  one fails to start.
- BroLink Host no longer rewrites the Windows power plan every few
  seconds; **Keep this PC awake while plugged in** now does exactly that.

### Stream

- Audio parameters and channel mappings from the PC are validated before
  any buffer is allocated or libopus is called.
- The encryption shim refuses overlapping buffers and writes padding to
  its output instead of the caller's input.
- A failed pairing restores the previous pairing state.
- Holding ⌘ and Ctrl together keeps Ctrl held on the PC when either is
  released.

### Interface

- Buttons, segments and toggles show a focus ring when reached from the
  keyboard, and toggles carry their label for assistive technology.
- Long details in lists show in full on hover.
- The host's **Stop the background service** button no longer freezes
  the window while it waits.

## 3.1.0 (2026-09-09)

Both apps now keep themselves current, the Mac says why a PC is slow and
asks the stream for what the path can carry, and the clipboard follows
you across.

### Updates

- The Mac checks GitHub about every six hours and twenty seconds after it
  starts (Settings has the switch and **Check now**). A newer app is
  downloaded, checked against the SHA-256 GitHub publishes and against its
  own code signature, moved over `/Applications/BroLink.app` once no
  stream is running, and relaunched.
- The Mac sends the new `brolink-host.exe` to every PC whose BroLink Host
  reports an older version, over the same Tailscale-authenticated control
  API that can put the PC to sleep (`POST /v1/update`). The host verifies
  the digest, that the bytes are a Windows executable and a newer version,
  swaps the file in beside itself and restarts; a running stream is not
  interrupted. A PC that is asleep gets it the next time the Mac sees it.
- A PC still on 3.0 has no update route. **PC → Update BroLink Host…** in
  the stream toolbar installs 3.1 through the stream: the Mac serves the
  executable on its own Tailscale address to that PC only, presses Win+R,
  types one PowerShell line and presses Enter; the script checks the
  SHA-256 baked into it, swaps the file and starts the new service. The
  PC's desktop has to be unlocked.

### Path and quality

- Each PC shows **Direct · 38 ms** or **Relayed via Tokyo · 210 ms**.
  Both apps run `tailscale netcheck`; when a PC is relayed, the lobby says
  which router is in the way and what would fix it (UPnP or NAT-PMP on
  the PC's router, a forwarded UDP port, or IPv6 on both ends).
- **Auto** quality, the default, picks resolution, frame rate and bitrate
  from the path right before connecting: 1080p/30/4 Mbps through a relay,
  1080p/60/8 across a long round trip, 1440p/60/15 nearby, 1440p/60/30 on
  a LAN. The toolbar's Quality menu switches between Auto, Smooth,
  Balanced and Sharp with a reconnect, and a dropped session offers **Try
  again at Smooth**.
- The decoder takes frames straight from the receive thread and accepts
  HEVC reference-frame invalidation, so a lost packet costs a repair frame
  rather than a keyframe. An explicit HEVC choice no longer falls back to
  H.264. Stats show packet loss, path and quality.
- BroLink Host reads which encoder Sunshine settled on from its log; the
  lobby and the host window warn when it is software encoding.

### Clipboard

- BroLink Host serves `/v1/clipboard` (text only, up to 32 KB). While
  streaming, text copied on the PC is in the Mac's clipboard within a
  second, and ⌘V on the PC first sends the Mac's text across, then
  presses Ctrl+V there. Notices under the toolbar say when something
  crossed.

### Keyboard and mouse

- A chord no longer releases a modifier the user is still holding, so ⌘C
  then ⌘V in one ⌘ hold both work.
- The keys macOS keeps for itself and what ⌘ does on the PC live in one
  **Keys** menu; the mouse mode is a menu that explains each choice.

### Staying reachable

- BroLink Host starts with Windows by default and sets that again at every
  service start unless the owner turned it off. It holds the PC awake
  while plugged in (a new setting, on by default; setup also zeroes the AC
  sleep timers). The Mac's offer to sleep the PC after a session is off by
  default.
- PCs are remembered with their Tailscale address and when they were last
  seen, so they stay listed while Tailscale on the Mac is down, and every
  machine whose Tailscale node key is due to expire gets a warning.

### Sound

- The Mac plays at the output device's own rate, channel count and sample
  format (48 kHz stereo f32 that the DAC refused was silence, with the
  reason only in the log). A short pre-roll avoids a click on the first
  packet; stats and a toast say where sound goes, or why it does not.
- BroLink Host reads Sunshine's log for a failed audio capture (no
  speakers, a missing sink). The lobby and the host window say so, with
  how to add a virtual device.

### Fixes

- Launched from Finder or the Dock, the Mac app reported "Tailscale is
  needed" with Tailscale running: the CLI inside Tailscale.app only answers
  when `SHLVL` is set.
- The Mac bundle has an icon.
- The Mac no longer POSTs the executable at a host older than 3.1 (which
  showed as a broken pipe); socket timeouts and closed connections read as
  such everywhere, and a push is retried on a transient error.
- The host's POST routes accept only JSON or an executable, so a web page
  open in a browser on the PC or on the Mac cannot sleep the PC, stop the
  service or replace its clipboard through a cross-origin form post.
- The Smooth preset (4 Mbps) survives a restart; the saved bitrate used to
  be raised to 5 Mbps on load.
- A downloaded app that fails its signature or version check is discarded
  and fetched again at the next check instead of failing the same way
  every fifteen minutes.
- Ctrl+Alt no longer toggles mouse capture every frame while the chord is
  held.
- VideoToolbox recreates its session after sleep/wake or a GPU reset
  instead of staying on a black picture until reconnect.
- An old BroLink Host window no longer kills a just-updated service in a
  loop; it relaunches itself from the new exe.
- Setup scripts are UTF-8 with a BOM, so a Korean Windows username or
  adapter name is not mangled, and the Sunshine installer path is quoted
  for msiexec.
- Holding a key repeats on the PC; a cancelled connect is not shown as an
  error; Sunshine's certificate changing (a reinstall) pairs again instead
  of asking the user to edit a file.

## 3.0.1 (2026-09-06)

- The host never entered the Mac's PIN on a release build of Sunshine;
  pairing talks to both Sunshine's current PIN API and the next one.
- `install-host.ps1` finds the executable beside the script and closes the
  panel first.

## 3.0.0 (2026-09-06)

- The GameStream client is compiled into the Mac app (moonlight-common-c,
  VideoToolbox, Opus); there is no Moonlight to install.
- The Windows zip carries Sunshine's installer; setup installs it offline.
- A stream toolbar: mouse capture, ⌘ mapping, a Keys menu, stats, full
  screen, the PC's power menu, Disconnect.
- Wake: Fast Startup off, driver keywords for sleep, modern standby and
  shutdown, a wake-packet listener on the host and **Test wake** on the Mac.
- License: GPL-3.0-or-later.

## 2.0.0 (2026-09-05)

- Built on Sunshine, Moonlight and Tailscale; BroLink keeps wake, pairing
  and power.

## 1.2.0 (2026-09-05)

- Mac fixes reviewed and verified on Windows; release preparation.

## 1.1.0 (2026-09-05)

- Both GUIs redesigned on a shared theme; Mac-reported issues fixed
  (colour, letterboxing, mouse release, audio, UI).
