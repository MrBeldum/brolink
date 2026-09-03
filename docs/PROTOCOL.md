# ForgeLink protocol

Version `FLK1` (byte `1`). Default UDP port **47850**.

ForgeLink is a 1:1 low-latency desktop streaming protocol. Video and audio
travel as unreliable datagrams (latest frame wins). Input and pairing travel
on the same UDP socket, encrypted after handshake.

## Datagram header (12 bytes)

| Offset | Size | Field |
|--------|------|--------|
| 0 | 4 | magic `FLK1` |
| 4 | 1 | version `1` |
| 5 | 1 | packet type |
| 6 | 4 | sequence (u32 LE) |
| 10 | 1 | flags |
| 11 | 1 | reserved |

Payload follows. After handshake the payload is ChaCha20-Poly1305 ciphertext
(16-byte tag appended). Nonce = `typ || 0x00×3 || seq_le || 0x00×4`. AAD =
`role_byte || typ`.

Hello, HelloAck, and Discovery are plaintext. Everything else is sealed.

Sequence numbers never wrap. Because the nonce is derived from
`(type, sequence)`, a wrap would reuse a nonce under the same key — the one
thing that breaks ChaCha20-Poly1305 outright. The sender refuses to send past
`u32::MAX` and the session ends; reconnecting derives fresh keys.

Receivers keep a 64-packet sliding replay window per direction and drop
anything already seen or too old.

## Packet types

| id | name | notes |
|----|------|-------|
| 1 | Hello | client identity + X25519 eph + quality hint |
| 2 | HelloAck | host identity + X25519 eph + `needs_pin` + Ed25519 signature |
| 3 | PairPin | 6-digit PIN |
| 4 | PairResult | ok / error |
| 5 | SessionReady | width, height, fps, codec, encoder |
| 6 | Video | fragmented Annex-B H.264 |
| 7 | Audio | 48 kHz s16 stereo PCM, 5 ms per packet |
| 8 | Input | mouse / key / XInput gamepad |
| 9 | Control | IDR request, capture mode, release-all-input |
| 10 | Ping | RTT |
| 11 | Pong | echo |
| 12 | Goodbye | |
| 13 | Discovery | LAN beacon (JSON) |

## Video payload

```
frame_id u32 | frag_idx u16 | frag_count u16 | timestamp_us u64 | data
```

Fragments are ≤ 1100 bytes so the whole datagram stays under 1200 bytes.
Incomplete frames are dropped; the next IDR (short GOP) recovers.

Every encoder is configured for **one slice per picture**, so a complete slice
NAL is a complete frame and the splitter can emit it immediately instead of
waiting for the next picture's first NAL. The receiver detects multi-slice
streams and warns, because that assumption would otherwise fail silently as
tearing.

## Audio payload

```
timestamp_us u64 | s16le stereo samples
```

5 ms per packet (480 stereo frames, 1920 bytes) so a packet plus header fits
one datagram with no fragmentation. The host resamples the WASAPI loopback
mix to 48 kHz; the client resamples again if its output device is not 48 kHz.

## Handshake

1. Client sends `Hello` to every candidate address in the ticket (LAN, STUN-WAN, Tailscale, relay), retransmitting every 400 ms for up to 8 s. The first datagram is also what punches the NAT hole, so losing it must not be fatal.
2. Host replies `HelloAck` signed by its persistent Ed25519 identity.
3. **The client checks the answering host's public key against the one in the ticket** and aborts if they differ, so a machine that has taken over the address cannot impersonate the host.
4. Both derive directional keys with HKDF-SHA256 over X25519(shared).
5. Unknown clients must enter a 6-digit PIN shown on the host. The public key is then stored in the allow-list.
6. Host starts capture/encode and sends `SessionReady`.

Connecting to a bare IP address instead of a ticket skips step 3 — there is no
pinned identity to check — and the client says so in its log.

## Tickets

Human-pasteable `flk1_` + unpadded base32 of:

```
version u8 = 2
host_id[32]
name_len u8 | name[name_len]
candidate_count u8
  { kind u8, ip[4], port u16 } * candidate_count
relay u8
  if relay: { ip[4], port u16, token[16] }
```

`kind` is `0` LAN, `1` WAN (STUN), `2` Tailscale. Version 1 tickets (a single
LAN and WAN address, no relay) still decode.

Candidates are tried LAN-first, then Tailscale, then WAN, then the relay: a
direct path is always faster when it works. Unspecified addresses and port-0
entries are dropped, and duplicates are removed.

The host refreshes its STUN mapping every 20 s while idle so the WAN port stays
open. It stops during a session, because reusing the media socket for STUN
would steal packets from the stream.

## NAT

- LAN: UDP broadcast / multicast discovery plus the ticket's LAN address. Beacons carry the full ticket, so a host found by discovery gets the same identity check as a pasted one.
- WAN: STUN (Google, then Cloudflare) reflexive address in the ticket. Most residential cone NATs work if the host keeps the mapping alive.
- Tailscale: a `100.64/10` address is advertised as its own candidate kind.
- Hard NAT: run `forgelink-relay` on a VPS and start the host with `--relay host:port`, or install Tailscale on both machines.

## Relay

Framing is `token[16] || payload`. The relay pairs the first two distinct
source addresses that present the same token and forwards each one's payload to
the other with the token stripped, so peers speak the ordinary protocol inside
the tunnel.

The relay never sees plaintext: everything inside is already authenticated and
encrypted end-to-end, so a relay operator can drop or delay traffic but cannot
read or forge it.

An empty payload is a keepalive that holds the NAT mapping open and is not
forwarded. A peer slot idle for 25 s can be claimed by a new address, so a NAT
rebinding recovers instead of being ignored. The session table is capped
(`--max-sessions`, default 512); under a flood of unknown tokens the relay
evicts half-open pairs before ever touching a session with two live peers.

## Input

Mouse: relative `dx/dy` from raw device motion while captured (games), absolute
0..65535 virtual-desktop coordinates when not.

Keyboard: Windows Set-1 scancode + virtual-key, injected with
`KEYEVENTF_SCANCODE`. Scancodes with the `0xE0` prefix are injected as extended
keys, so the arrow keys do not act like the numeric keypad.

Modifiers are sent as key events on state transitions, because the client
toolkit reports them as a bitmask rather than as keys.

Both ends track what is held. Releasing mouse capture, disconnecting, or the
host losing the session releases every held key and button, so a dropped
connection cannot leave a key stuck down on the host.

Gamepad: XInput-compatible report, injected through ViGEmBus when installed.
Sent only when the pad state changes.
