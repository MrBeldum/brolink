# Changelog

The release workflow publishes the section that matches the tag as the
GitHub release notes, so each version gets a heading of the form
`## X.Y.Z (date)`.

## 4.0.2 (2026-09-22)

- `panel.log` and `service.log` no longer grow without end. Both were
  opened in append mode and nothing ever pruned them, so a machine that
  streams for weeks kept every line it had ever logged: Hermes reached a
  32 MB `panel.log`. Each file now rolls over at 4 MB, keeping one previous
  generation as `<name>.1`, so a log costs at most 8 MB however long the
  service or the window runs.
- The window no longer logs a line for every frame it draws. wgpu's Vulkan
  backend warns whenever a presented frame's swapchain reports suboptimal,
  which on a Windows PC is every frame; it was every single line of that
  32 MB file. The condition is normal and wgpu recreates the swapchain
  itself, so that one target is now heard from only when it is an error.
  `RUST_LOG` still overrides everything.

## 4.0.1 (2026-09-21)

- The streaming engine on a Windows PC no longer owns a terminal window.
  The helper that runs it as the signed-in user started it as a plain
  console program, so a console sat on the streamed desktop and closing it
  stopped the engine and the stream with it; the engine is now created
  with its console hidden. The logon task that runs the helper no longer
  goes through a `.cmd`, which flashed a terminal at every logon and engine
  restart; it runs through a windowless script host instead.
- The bitrate the PC's encoder targets is now the number chosen. Sunshine
  takes 20% off the request for FEC and then 512 kbps for audio and
  500 kbps for control traffic, on top of moonlight's own 20%, so a 20 Mbps
  target was encoding at 18988 kbps. The request now compensates for the
  whole chain.

## 4.0.0 (2026-09-20)

- On macOS and Linux the background service keeps its login item
  registered without restarting itself: it only writes the launch agent or
  user unit, and a copy started while another already answers on the port
  exits quietly instead of failing, so launchd no longer respawns a
  duplicate every ten seconds for the rest of the session and a service
  launchd started at login is no longer killed by its own registration.
  Turning the login item off in the panel leaves the running service alone.
- Only a Windows PC accepts a pushed `brolink-host.exe`; a Mac or a
  container answers 400 instead of trying to swap it in, and the Mac only
  sends one to Windows machines.
- Elevated Windows setup approved with another administrator's password
  reads and writes the launching user's BroLink folder, so the engine login
  it configures is that user's.
- The updater takes the window's locks in the window's order; the reverse
  could hang the app when an update was ready just as a stream ended.

- Docker desktops now publish a valid streaming app catalog. The missing
  environment object made a healthy-looking VPS report "offers nothing to
  stream"; node self-tests now check the catalog and persisted login setup.
  Startup waits for the engine before launching the control service, which
  prevents two engine processes racing to bind the same ports.

- On Windows, the streaming engine runs as the signed-in user and the
  only active speakers stay the default. The engine's own startup was
  clearing Steam Streaming Speakers on a PC with no other playback
  device, which left `Couldn't get default audio endpoint` and a silent
  stream. Startup probe errors are no longer reported as a missing sound
  device.

- Software encode (a VPS with no GPU) uses every CPU core and the
  ultrafast preset so 1080p60 holds instead of collapsing to ~30 fps and a
  trickle of bitrate. AMD encode prefers speed, with preanalysis off, so
  the target rate does not sit at ~18 Mbps on a still desktop.
- The stream protocol always treats Tailscale as a LAN: no 1024-byte
  packets, no extra 500 kbps tax, and the bitrate is sent at launch as
  well as over RTSP.
- The streaming engine is not a second app. Status, logs, the Dock and
  the app list say BroLink; only Desktop is launched.

- BroLink is a mesh. Every install lists every other machine on the
  Tailscale account — Windows, Mac, Linux, a VPS container — and Connect
  opens that desktop. The same app shares this machine: Mac and Linux
  install the streaming engine; Windows still does. There is no designated
  host or client role.
- `deploy/node/` is a Docker kit for a VPS or any container: a virtual
  desktop, the streaming engine, BroLink's control service, and optional
  Tailscale userspace so the container is its own machine. Several copies
  on one host are several machines. The packet relay stays `deploy/relay/`.
- Stream bitrate holds the target instead of swinging with the scene.
  Sunshine is set to constant bitrate (AMD CBR+HRD, NVENC VBV, VA-API CBR),
  Tailscale is treated as a LAN so moonlight does not tax 500 kbps or shrink
  packets to 1024, packets stay 1184 bytes for WireGuard's MTU, and the
  moonlight cap is 150 Mbps. Recommended quality is 50 Mbps; Sharp is 100.
- Settings and pairing no longer wipe each other. The window, discovery and
  pairing used to rewrite all of `client.toml` from a stale copy, so a
  bitrate change could drop the PC's certificate and its wake details, and
  a reconnect right after the first pair could ask for a PIN again. Every
  write is now one read-modify-write under a lock: the window overlays its
  preferences, discovery and pairing edit only what they learned, and the
  host panel does the same with `host.toml`. Config files are written 0600
  in a 0700 folder, and a damaged file is kept beside the new one as
  `.bad-*` instead of being replaced.
- An update no longer installs while a connect is waking, pairing or
  launching, a connect refuses to start while an install is under way, and
  a cancelled connect cannot finish into the next one. Cancel takes effect
  while the PC is still being asked for the PIN. The update cache keeps
  this release and the previous one only.
- Connecting no longer overwrites the Mac clipboard with whatever the PC
  already held. Ctrl+Alt (the host key) is never sent to the PC, and it
  works even when a control has focus; while the mouse is captured, keys go
  to the PC rather than to a focused control. Captured mouse movement is
  scaled to the stream's pixels, so a hand movement crosses the same share
  of the PC's desktop as of the picture.
- Every Tailscale CLI call is bounded (10 s; the per-request `whois` 2 s),
  and its output is drained while it runs, so a slow or chatty CLI cannot
  hang the window or the control service.
- Host setup runs `brolink-host --setup-elevated` after UAC, writes its
  script under `%SystemRoot%\Temp` (administrators only) and passes the
  engine take-over helper as an encoded command, instead of running a
  user-writable `setup.ps1`. Repair setup registers the engine service when
  files exist but the service is down, and stops a leftover service before
  deleting it. Firewall `program=` paths are single-quoted so a `$` in the
  username is not expanded. The logon task for the take-over helper runs
  with limited rights.
- The engine login is written as the engine's own hashed credentials file
  (`brolink-web.json`, `credentials_file =` in `sunshine.conf`) instead of
  `sunshine --creds` on a command line, and the engine's web API is called
  with the password on stdin (`curl --config -`), so neither the setup log
  nor the process list shows it. The Docker node does the same through
  `deploy/node/bootstrap.py`, which also keeps an existing `host.toml`
  (preferences, pairing) intact across restarts.
- Auto-update refuses a GitHub asset with no SHA-256. The Mac installer
  checks that digest, verifies the signature, and swaps `/Applications`
  without deleting the live app first. `git credential` is killed if its
  stdin closes early.
- Stream teardown waits for in-flight input and keeps the session alive
  until moonlight's detached termination thread has nowhere to call; a
  decode unit whose buffer list overruns its declared length is refused.
- The migration check reads the installed engine's real kind instead of
  labelling every install "BroLink". The macOS launch agent and Linux user
  unit escape the executable path, and a failure to register them is
  reported instead of ignored. Spawned engine and service processes are
  reaped. A settings save failure shows in the window.

- One pointer. Freeing the mouse showed the Mac cursor behind egui's back,
  and it stayed on top of the PC's own cursor in the picture until the
  pointer left the window; the cursor is now hidden the one way egui
  keeps track of.
- The mouse is captured by default: a click on the picture hides the
  cursor, holds it in place and sends raw movement, which is what games
  read for the camera. A game that ignores the cursor's position (Genshin
  Impact) turned only in the old Captured mode; Honkai: Star Rail happened
  to follow the position too. **Free** in the Mouse menu is the old
  behaviour, and the choice is remembered.
- The stream is the whole window, nothing over it. **Ctrl+Alt** is the
  host key, as in a hypervisor: it frees the mouse and drops the toolbar
  over the picture; a click on the picture, or Ctrl+Alt again, puts it
  away and takes the mouse back. The toolbar no longer sits above the
  picture in a window, which is what left the bars either side, and a
  window takes the stream's proportions when a session starts.
- 1080p, 1440p and 4K are 1920×1080, 2560×1440 and 3840×2160, not those
  widths at this display's shape (1920×1246 on a 14" MacBook Pro), and the
  list shows the pixel size of each. Match screen is unchanged. On a
  display of another shape the standard sizes leave a bar above and below.
- Less delay between the PC and the eye. A decoded frame is presented as
  soon as it is drawn instead of waiting for the display's next refresh,
  up to a refresh interval sooner, and the decoder fills the same few
  buffers over and over instead of taking nine fresh megabytes per frame.
- The Dock icon is the bundle icns, in macOS's rounded app-icon shape.
  eframe was replacing it at runtime with a 64-pixel square of the logo,
  and the icns had been pre-masked with transparent corners, which macOS
  26 draws as a square plate. `scripts/make-macos-icon.py` now fills the
  canvas and lets the system apply the shape.
- The Windows host icon (Explorer, taskbar, Alt-Tab, the window, and the
  in-app header) is the same filled tile. The logo's outer pad had left a
  square plate around the mark; `icon::render` now crops it.

## 3.3.0 (2026-09-15)

- The stream is the Mac's screen. **Match screen** is the default: the PC
  is asked for this display's own pixel size, and 1080p, 1440p and 4K now
  cap the long edge while keeping the display's proportions, so nothing is
  letterboxed or stretched. The engine switches the PC's display to the
  size asked for (setup turns its display settings on, and the launch
  request now allows it), and a session reconnects by itself when the Mac
  moves to a different display.
- Nothing lowers the quality for the route any more. Recommended quality
  is 35 Mbps at this screen's size whether the path is direct, through
  your relay or through Tailscale's; a long round trip adds delay, not a
  bitrate or frame-rate cap. The engine's own bitrate ceiling is cleared
  and it is asked to keep frames flowing while the desktop is still. A PC set up
  by an earlier BroLink gets the same profile applied once by the service,
  between sessions. The quick profiles are now Smooth (1080p · 60 fps ·
  12 Mbps), Balanced (match screen · 60 · 35) and Sharp (match screen ·
  60 · 65).
- A PC whose only display is the Virtual Display Driver can only switch to
  sizes that driver lists. Setup adds every size a Mac can ask for to its
  `vdd_settings.xml` (a minimal file when there is none, everything else
  kept) and restarts the display; the setup card says when sizes are
  missing.
- The Mac decoder is checked for hardware acceleration rather than assumed,
  runs in real-time mode, decodes off the packet-receive thread, and the
  window presents with one frame of latency. Stats name the decoder as
  hardware or software.
- One **Stream settings** panel, in the lobby and during a session: quick
  profiles, picture quality, resolution, frame rate, bitrate target and
  video format, with the exact size, rate and target it adds up to.
  Applying mid-stream reconnects. **Stats** opens a Stream performance
  window (received against target bitrate and frame rate, round trip,
  loss, host, assembly, queue and decode times, decoder) with a copy
  button; the status line always shows achieved against requested. The
  lobby leads with your PCs and a Next session summary; connection
  details fold away. The host panel has an Overview and a Settings view,
  a Repair setup button and a Diagnostics fold with a copy-log button.

## 3.2.0 (2026-09-15)

- A private Tailscale peer relay you run yourself. `deploy/relay/` is a
  Docker kit for a VPS (UDP 40000, iptables above the cloud REJECT, state
  volume, wait-for-Running before `tailscale set`). Tailscale's own DERP
  stays the fallback; BroLink adds no relay protocol of its own. Settings
  shows a Relay card with the exact state: no relay on this network, a
  relay that this device is not granted, or ready. Needs Tailscale 1.86 or
  later on every device.
- Streams tell a peer relay apart from DERP. Auto quality on a peer relay
  goes up to 1440p60 at 40 Mbps by round trip; the DERP and direct tiers are
  unchanged. Tagged relay nodes never appear in the PC list.
- The Windows streaming engine now unpacks from the pinned lite archive to
  `C:\Program Files\BroLink\engine` as the "BroLink Streaming" service:
  no Apps & Features entry, Start Menu shortcut, tray icon or web-UI link.
  Firewall rules are named "BroLink". An engine installed earlier by an
  MSI is migrated in order — copy state, verify, start, prove it listens,
  then uninstall the MSI — and keeps its pairing and web login; a
  `config.bak` is left beside it and the old install folder is removed
  once the MSI is gone. The engine's processes appear as "BroLink
  Streaming" with BroLink's icon in Task Manager, the volume mixer and
  firewall prompts: setup rewrites their version block and icon in place
  and keeps their copyright and licence strings. The host offers
  **Update this PC** after
  an exe-only update and refuses a self-update while a stream is running.
- A PC whose engine certificate changed asks to pair again instead of a
  generic TLS error, and the old pin is forgotten.
- No user-visible mention of upstream project names or ports; **Open
  source** in Settings shows the notices.
- Nothing overlaps at the smallest window (640×420 on the Mac, 560×600 on
  the PC): the stream toolbar folds into a More menu, overlays shrink to the
  window, and CPU-only layout tests enforce it.
- New logo and palette (violet, pink and cyan on near-black) across the
  Dock, Finder, Windows and in-window icons.
- A connected stream that remains black now says so and asks the PC why,
  instead of presenting a healthy-looking stream. Missing video, frozen
  video, and decoder failures have separate persistent notices.
- The notice offers **Restart stream**, which starts a fresh Desktop capture
  instead of resuming the same broken capture. Failure to stop Sunshine's
  previous session is reported instead of silently resuming the wrong app.
- Frame rate and bitrate no longer remain frozen at their last good values
  when video stops. Stream overlays no longer pass clicks through to the PC.
- The stream's audio line says when the output device is taking no sound,
  instead of naming a device that is silent.
- VideoToolbox callback errors now request a recovery frame and recreate
  failed decoder sessions, just like synchronous decode errors.
- The PC's display report now answers why a capture is black: whether any
  monitor hardware is present, whether the session is locked, how bright the
  PC's own desktop is (as sampled numbers, never an image), which colour
  mode Windows is composing in, and Sunshine's capture settings and log
  tail. Each part reports its own failure rather than losing the whole
  report.
- A PC whose monitor is gone keeps composing in the colour mode that
  monitor asked for — half-float, ten bits a channel — on a placeholder
  display that reports no luminance at all. A capture converting that to an
  ordinary picture turns every frame black while the PC's own desktop draws
  normally. The stream's notice now says so, in place of blaming a missing
  display for a desktop that is plainly there.
- Where that mode is one the display supports, the notice offers to turn it
  off and the Mac does so over the control API, then starts a fresh
  capture. Windows 11 splits that switch into HDR and wide colour, so each
  is tried in turn and the display is read back afterwards rather than the
  return code believed.
- Where Windows is enforcing the mode on a placeholder display it refuses
  every switch with ERROR_NOT_SUPPORTED — the mode is a consequence of
  having no display, not a setting. The notice says that plainly and asks
  for a monitor, a dummy plug or a virtual display driver instead of
  offering a button that cannot work.
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
