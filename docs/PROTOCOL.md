# BroLink protocol

Version `BLK1` (byte `1`). Default UDP port **47850**.

BroLink is a 1:1 low-latency desktop streaming protocol. Video and audio
travel as unreliable datagrams (latest frame wins). Input and pairing travel
on the same UDP socket, encrypted after handshake.

## Datagram header (12 bytes)

| Offset | Size | Field |
|--------|------|--------|
| 0 | 4 | magic `BLK1` |
| 4 | 1 | version `1` |
| 5 | 1 | packet type |
| 6 | 4 | sequence (u32 LE) |
| 10 | 1 | flags |
| 11 | 1 | reserved |

Payload follows. After handshake the payload is ChaCha20-Poly1305 ciphertext
(16-byte tag appended). Nonce = `typ || 0x00×3 || seq_le || 0x00×4`. AAD =
`role_byte || typ`.

Hello, HelloAck, Discovery, and the rendezvous types are plaintext.
Everything else is sealed.

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
| 5 | SessionReady | width, height, fps, codec, encoder, host name, wake MAC, power-control flag |
| 6 | Video | fragmented Annex-B H.264 |
| 7 | Audio | 48 kHz s16 stereo PCM, 5 ms per packet |
| 8 | Input | mouse / key / XInput gamepad |
| 9 | Control | IDR request, capture mode, release-all-input, clipboard, power |
| 10 | Ping | RTT |
| 11 | Pong | echo |
| 12 | Goodbye | |
| 13 | Discovery | LAN beacon (JSON) |
| 14 | Register | host → rendezvous: signed key, name, candidates, cookie |
| 15 | RegisterAck | rendezvous → host: observed address, TTL |
| 16 | Lookup | client → rendezvous: signed host key, nonce, cookie; padded to 512 B |
| 17 | LookupAck | rendezvous → client: host record or "unknown" |
| 18 | Punch | rendezvous → host: client address + nonce |
| 19 | PunchProbe | host → client: host key + nonce, opens the host's NAT |
| 20 | Retry | rendezvous → either: "resend with this cookie" |

Types 14–20 are never sealed: they belong to no session and are signed
(Register, Lookup) or bind to a signed request's nonce instead.

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

1. Client sends `Hello` to every candidate address in the ticket (LAN, STUN-WAN, Tailscale, relay), retransmitting every 400 ms for up to 8 s. Losing it must not be fatal, and it opens the *client's* own NAT for the reply -- it does not open the host's. `Hello` may include `client_wan`, the client's own STUN address, so the host can send a packet back and finish a hole punch.
2. Host replies `HelloAck` signed by its persistent Ed25519 identity.
3. **The client checks the answering host's public key against the one in the ticket** and aborts if they differ, so a machine that has taken over the address cannot impersonate the host.
4. Both derive directional keys with HKDF-SHA256 over X25519(shared).
5. Unknown clients must enter a 6-digit PIN shown on the host. The public key is then stored in the allow-list.
6. Host starts capture/encode and sends `SessionReady`.

Connecting to a bare IP address instead of a ticket skips step 3 — there is no
pinned identity to check — and the client says so in its log.

## Tickets

Human-pasteable `blk1_` + unpadded base32 of:

```
version u8 = 2          (IPv4-only; still emitted when every address is v4)
host_id[32]
name_len u8 | name[name_len]
candidate_count u8
  { kind u8, ip[4], port u16 } * candidate_count
relay u8
  if relay: { ip[4], port u16, token[16] }
```

Version 3 is emitted when any address is IPv6:

```
version u8 = 3
…same header…
  { kind u8, family u8 (4|6), ip[4|16], port u16 }
relay: family u8 | ip | port u16 | token[16]
```

`kind` is `0` LAN, `1` WAN (STUN / UPnP / global v6), `2` Tailscale. Version 1
tickets (a single LAN and WAN address, no relay) still decode.

Candidates are tried LAN-first, then Tailscale, then WAN, then the relay: a
direct path is always faster when it works. Unspecified addresses and port-0
entries are dropped, and duplicates are removed.

The host refreshes its STUN mapping every 20 s while idle so the WAN port stays
open. It stops during a session, because reusing the media socket for STUN
would steal packets from the stream. UPnP mappings are refreshed on a longer
timer (about 15 minutes) and do not use the media socket for SSDP.

The host is reactive by default — it replies to the address a packet arrived
from. Two exceptions make worldwide access work without a signalling server:
UPnP/NAT-PMP (the router forwards the port) and `Hello.client_wan` (the host
also sends `HelloAck` to the client's STUN address). The UPnP lease is
requested as permanent (NAT-PMP: seven days) so a PC that is asleep for a
long weekend can still be woken from outside.

## Rendezvous

When the ticket names a relay, the same address also answers rendezvous
requests on the media socket, unframed (the relay tells them from relay
frames by the `BLK1` magic; relay tokens never start with it).

1. The host sends `Register` every 20 s while idle: its key, name, and ticket
   candidates, signed with its Ed25519 identity and timestamped (±120 s).
   The first attempt has no cookie; the coordinator answers `Retry` with an
   HMAC cookie bound to the source address and a 60 s slot, and the host
   resends with it. Registrations expire after 90 s.
2. Connecting, the client sends `Lookup` alongside its first `Hello`s (same
   cookie dance). `LookupAck` returns the host's registered candidates and
   the address the coordinator observed the registration from: the host's
   live WAN mapping. The client adds those and sends `Hello` there at once.
3. At the same moment the coordinator sends `Punch` to the host, which fires
   two `PunchProbe`s at the client's address. That opens the host's NAT for
   the client, and a client that sees the probe (matched by nonce) knows which
   path is live and knocks there immediately.
4. Direct paths that fail leave the relay, whose token is already in the
   ticket.

A hostile coordinator can delay or misdirect an introduction; it cannot
impersonate the host (the client still checks the handshake signature against
the pinned key), forge a registration, or make the host probe a spoofed
address (the cookie proves the source).

## Wake and power

`SessionReady.wake_mac` carries the MAC of the host's LAN adapter; the client
stores it with the saved PC. A magic packet (`FF×6` then the MAC ×16) is sent
to the LAN broadcast, the LAN address on ports 9 and 47850, the /24 directed
broadcast, and every WAN candidate on its ticket port. The NIC matches the
pattern regardless of port, so the existing router mapping carries it.

`ControlMsg::Power { action }` asks for `Sleep`, `Hibernate`, `Restart`, or
`Shutdown`. The host answers with `Goodbye`, tears the session down (ffmpeg
stopped, held keys released), waits a second, and acts. It only does so when
`SessionReady.power_control` was true, which mirrors the host's
**allow_power_control** setting.

## NAT

- LAN: UDP broadcast / multicast discovery plus the ticket's LAN address. Beacons carry the full ticket, so a host found by discovery gets the same identity check as a pasted one.
- UPnP / NAT-PMP: the host asks the gateway to forward its UDP port and advertises the mapped address as WAN. This is the default internet path on a cooperative home router.
- IPv6: globally-routable addresses are advertised as WAN candidates. No NAT, so they work from anywhere the client's ISP has v6.
- WAN (STUN): Google, then Cloudflare, reflexive address in the ticket. Without UPnP this keeps the *mapping* alive, but delivery of the client's first packet depends on the router's *filtering*: only endpoint-independent filtering ("full cone") accepts it. Treat a STUN-only WAN candidate as an optimisation that sometimes works.
- Hole punch: the client puts its own STUN address in `Hello.client_wan`. The host also sends `HelloAck` there so a restricted-cone client NAT sees a packet from the host.
- Tailscale: a `100.64/10` address is advertised as its own candidate kind.
- Hard NAT / CGNAT: run `brolink-relay` on a VPS and point the host at it, or install Tailscale on both machines.

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

Clipboard: `ControlMsg::Clipboard { text }` in both directions, capped so the
JSON still fits one datagram. Each side ignores a paste it just applied, so
the two clipboards cannot oscillate.
