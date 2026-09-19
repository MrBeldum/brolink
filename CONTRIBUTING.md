# Contributing

## Building

Rust stable, a C compiler (MSVC Build Tools on Windows, Xcode command line
tools on macOS) and CMake. The C compiler builds the vendored
moonlight-common-c; CMake builds libopus for the audio decoder.

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`BROLINK_SKIP_C=1` skips the C build for a quick cross-target check, for
example `BROLINK_SKIP_C=1 cargo check -p brolink-client --target
aarch64-apple-darwin` from Windows after `rustup target add`.

## Where things live

- `crates/stream`: everything that talks to Sunshine. `nvhttp` is the
  pairing and launch API, `session` drives moonlight-common-c through the C
  shim in `csrc/`, `video` and `audio` decode. It knows nothing about
  windows or Tailscale.
- `crates/client`: the viewer UI (a library plus `brolink-client`).
  `session.rs` is the path from a listed machine to a live stream;
  `stream.rs` is the toolbar and input while streaming; `video.rs` draws
  frames with wgpu.
- `crates/host`: the node (`brolink-host`): control service, engine setup,
  and the unified window that both views other machines and shares this
  one. Windows administrator work stays in the generated setup script in
  `setup.rs`. macOS/Linux setup is `unix_setup.rs`.
- `crates/core`: the control API, a small HTTP client and server, the
  Tailscale CLI wrapper and wake packets.
- `crates/ui`: one palette, one typeface, one set of cards, rows, pills and
  buttons. New visual elements go there rather than inline.

The control API (`crates/core/src/api.rs`) is read by older clients and
hosts at times: new JSON fields get `#[serde(default)]` and existing ones
keep their meaning.

## Seeing the UI without a Mac or a PC

```bash
cargo test -p brolink-client -p brolink-host snapshots -- --ignored
```

renders every screen to `target/ui-snapshots/*.png`.

## Tests against a real Sunshine

With Sunshine running on the development PC and BroLink Host's service
started (`cargo run -p brolink-host -- --background`):

```bash
cargo test -p brolink-stream pair_real -- --ignored --nocapture     # five-phase pairing
cargo test -p brolink-stream stream_real -- --ignored --nocapture   # ten seconds of video
BROLINK_DEV_LOCAL=1 cargo test -p brolink-client stream_snapshot -- --ignored --nocapture
cargo test -p brolink-client wake_test_real -- --ignored            # wake packet round trip
```

`BROLINK_DEV_LOCAL=1` also makes the client app list this PC at 127.0.0.1,
so `cargo run -p brolink-client` on Windows streams the PC to itself for
trying the toolbar and input.

The Windows client decodes with OpenH264 in software; that path exists for
development only. The Mac path (VideoToolbox) is the one that ships.

On a Mac, the snapshot test can use an already saved PC and its existing
pairing. It verifies decoded video, renders the stream, and disconnects:

```bash
BROLINK_TEST_PC='Gaming-PC' cargo test -p brolink-client stream_snapshot -- --ignored --nocapture
cargo test -p brolink-client decoded_frame_reaches -- --ignored
```

`BROLINK_TEST_CODEC=h264` or `hevc` selects the codec. A black capture fails
the live test by default; `BROLINK_TEST_EXPECT_BLACK=1` instead verifies the
persistent black-picture diagnosis when testing a PC without an active
display. The GPU test uses a synthetic NV12 image to check that the picture
is drawn with the correct orientation and range and retained between frames.
