# Running a relay

`brolink-relay` is one small binary on one UDP port that does two jobs:

1. **Relay.** When neither side can reach the other directly (CGNAT, a
   locked-down router, a hotel network), both send to the relay and it
   forwards between them. It only ever sees ciphertext.
2. **Rendezvous.** The host registers its public key and current address
   every 20 s. A client with an old ticket asks "where is this key now?",
   gets the live address, and the relay tells the host to punch a hole
   towards the client. This is what keeps a ticket working after the home
   IP changes, and what makes direct connections succeed through the common
   port-restricted home NAT.

You need a machine with a public IP and UDP port 47851 open. The cheapest
VPS tier is plenty: a session that falls back to the relay uses the stream's
bitrate in bandwidth and almost no CPU.

## Build and run

```bash
cargo build --release -p brolink-relay
./target/release/brolink-relay --bind 0.0.0.0:47851
```

Flags: `--max-sessions` (default 512) caps relayed pairs, `--max-hosts`
(default 10000) caps registrations. Both bound memory under a flood; nothing
else is configurable because nothing else needs to be.

Then in **BroLink Host**, set **Relay** to `your.vps.example:47851`. The
address and a per-host token go into the ticket; the Mac needs nothing.

## systemd

`deploy/brolink-relay.service`:

```bash
sudo cp target/release/brolink-relay /usr/local/bin/
sudo cp deploy/brolink-relay.service /etc/systemd/system/
sudo systemctl enable --now brolink-relay
sudo ufw allow 47851/udp   # or your firewall's equivalent
```

## Docker

```bash
docker build -f deploy/Dockerfile -t brolink-relay .
docker run -d --restart unless-stopped -p 47851:47851/udp brolink-relay
```

## What it can and cannot see

The relay learns which host key is registered from which address, and which
client address asked for it. That is the metadata any introduction service
has. It never holds a key that decrypts the stream, cannot impersonate a host
(registrations are signed by the host's identity, and the client checks the
host's signature against the key in its ticket), and cannot make a host send
packets to a spoofed address (every request must echo a cookie the relay
issued to that exact source).
