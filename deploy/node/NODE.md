# BroLink node (Docker)

A container that **shares a desktop** on your Tailscale network. Any BroLink
app — Mac, Windows, or another node — lists it and can Connect.

Use this for a VPS, or run several copies on one host so each container is
its own machine (its own Tailscale identity, its own desktop).

It is not the packet relay. The relay kit stays in `deploy/relay/`.

## What runs inside

- Xvfb + XFCE (a virtual 1920×1080 desktop)
- Sunshine (the same streaming engine BroLink uses on Windows and macOS)
- BroLink's control service on TCP 47850
- Optional Tailscale in userspace mode (`TS_AUTHKEY`)

## One node

From the BroLink repository:

```bash
cp deploy/node/env.example deploy/node/.env
# edit .env: paste a Tailscale auth key, pick TS_HOSTNAME
docker compose -f deploy/node/docker-compose.yml up -d --build
```

Sign the auth key in at [the admin console](https://login.tailscale.com/admin/machines)
if it asks. Open BroLink on your Mac or Windows PC: the hostname appears
under **Your machines**. Click **Connect**.

## Several containers on one VPS

Each compose project needs its own name and hostname:

```bash
BROLINK_CONTAINER=brolink-node-a TS_HOSTNAME=vps-a \
  docker compose -p node-a -f deploy/node/docker-compose.yml up -d --build

BROLINK_CONTAINER=brolink-node-b TS_HOSTNAME=vps-b \
  docker compose -p node-b -f deploy/node/docker-compose.yml up -d --build
```

Give each a distinct `TS_AUTHKEY` (or the same reusable key; Tailscale
issues a new node per `up`). Do not put `tag:relay` on these nodes.

## Same host as the peer relay

The relay container uses host networking and `tag:relay`. A node container
should **not** use host networking: userspace Tailscale gives it a second
identity, so it shows up as a machine instead of being hidden as a relay.

## Build notes

The image compiles `brolink-host` and installs the Ubuntu 24.04 Sunshine
package pinned to the same tag Windows setup uses (`v2026.906.222525`).
The VPS in this project is aarch64; the Dockerfile picks the arm64 or
amd64 `.deb` from `TARGETARCH`.
