# Contributing

BroLink is the small part: waking, pairing, power, setup. If a change
would make BroLink capture, encode, decode, relay, or authenticate anything
itself, it belongs in Sunshine, Moonlight or Tailscale instead.

Before opening a PR:

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The control API (`crates/core/src/api.rs`) is read by an older client or
host at times, so new JSON fields get `#[serde(default)]` and existing
ones keep their meaning.

Both windows are built from `crates/ui`: one palette, one typeface, one set
of cards, rows, pills and buttons. Put new visual elements there rather than
styling them inline. To see what a change looks like without a Mac or a PC:

```bash
cargo test -p brolink-client -p brolink-host snapshots -- --ignored
```

The PNGs land in `target/ui-snapshots/`.

To exercise the host service on a Windows box with Tailscale:

```powershell
cargo run -p brolink-host -- --background
curl http://127.0.0.1:47850/v1/status
```

Anything that needs administrator rights lives in the generated setup
script (`crates/host/src/setup.rs`), so it runs behind one UAC prompt and
stays readable as PowerShell.
