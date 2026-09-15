#!/bin/sh
# BroLink peer-relay entrypoint.
#
# The official tailscale image entrypoint (containerboot) exposes no relay setting
# and does not run a user command, so relay prefs have to be applied with
# `tailscale set` after the node is authenticated. This wrapper does that from a
# background subshell and then execs containerboot as PID 1, so signals, restarts
# and logs behave exactly like the stock image.
#
# Never `set -x` here: TS_AUTHKEY is in this process environment.
set -eu

RELAY_PORT="${RELAY_PORT:-40000}"
RELAY_WAIT_SECS="${RELAY_WAIT_SECS:-180}"
RELAY_STATIC_ENDPOINTS="${RELAY_STATIC_ENDPOINTS:-}"
# Peer relay is GA from this client version; older clients silently lack the flag.
RELAY_MIN_MAJOR=1
RELAY_MIN_MINOR=86

log() { echo "brolink-relay: $*" >&2; }

# 0 if $1 (e.g. "1.102.3") is >= RELAY_MIN_MAJOR.RELAY_MIN_MINOR.
# Numeric on purpose: a string compare would rank "1.102" below "1.86".
version_ok() {
	echo "$1" | awk -F. -v maj="$RELAY_MIN_MAJOR" -v min="$RELAY_MIN_MINOR" \
		'{ exit !(($1+0) > maj || (($1+0) == maj && ($2+0) >= min)) }'
}

# Print the value to pass to --relay-server-static-endpoints, or nothing.
# $1 = an IP literal, $2 = port. IPv6 needs brackets; anything unparseable is dropped
# rather than passed through, so a captive-portal HTML body can't become an endpoint.
format_endpoint() {
	case "$1" in
	'') ;;
	*:*) case "$1" in *[!0-9a-fA-F:]*) ;; *) echo "[$1]:$2" ;; esac ;;
	*[!0-9.]*) ;;
	*.*.*.*) echo "$1:$2" ;;
	esac
}

# The public IP is resolved at start rather than baked into .env, because a cloud
# instance without a reserved address gets a new one on every stop/start.
# Only busybox wget is present in the image; there is no curl.
# A dual-stack host answers ifconfig.me over IPv6 and would advertise only that,
# so each family is asked through a single-family host (busybox wget has no -4/-6).
public_ip() { # $1 = 4 | 6
	if [ "$1" = 4 ]; then
		wget -qO- -T 5 https://api.ipify.org 2>/dev/null ||
			wget -qO- -T 5 https://ipv4.icanhazip.com 2>/dev/null ||
			true
	else
		wget -qO- -T 5 https://api6.ipify.org 2>/dev/null ||
			wget -qO- -T 5 https://ipv6.icanhazip.com 2>/dev/null ||
			true
	fi
}

static_endpoints() {
	if [ -n "$RELAY_STATIC_ENDPOINTS" ]; then
		echo "$RELAY_STATIC_ENDPOINTS"
	else
		v4=$(format_endpoint "$(public_ip 4 | tr -d '[:space:]')" "$RELAY_PORT")
		v6=$(format_endpoint "$(public_ip 6 | tr -d '[:space:]')" "$RELAY_PORT")
		case "$v4,$v6" in
		,) ;;
		,*) echo "$v6" ;;
		*,) echo "$v4" ;;
		*) echo "$v4,$v6" ;;
		esac
	fi
}

# Bounded wait. Applying relay prefs before the backend is Running is a silent no-op,
# and a login URL can sit unvisited for a long time, so this has to poll, not sleep once.
wait_for_running() {
	i=0
	while [ "$i" -lt "$RELAY_WAIT_SECS" ]; do
		state="$(tailscale status --json 2>/dev/null |
			sed -n 's/.*"BackendState"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
		case "$state" in
		Running) return 0 ;;
		NeedsLogin)
			# `[ ] && log` would abort this subshell under `set -e` on the false branch.
			if [ "$((i % 30))" -eq 0 ]; then
				log "waiting for login (docker logs brolink-relay | grep login.tailscale.com)"
			fi
			;;
		esac
		i=$((i + 1))
		sleep 1
	done
	log "BackendState never reached Running within ${RELAY_WAIT_SECS}s (last: ${state:-unknown})."
	log "Relay prefs NOT applied. Authenticate, then: docker restart brolink-relay"
	return 1
}

configure_relay() {
	version="$(tailscale version 2>/dev/null | head -n 1)"
	if ! version_ok "${version:-0.0}"; then
		log "tailscale ${version:-unknown} is older than ${RELAY_MIN_MAJOR}.${RELAY_MIN_MINOR}; peer relay unavailable. Relay prefs NOT applied."
		return 1
	fi
	wait_for_running || return 1

	endpoints="$(static_endpoints)"
	# `if configure_relay` disables set -e for this whole function (POSIX). A
	# failing `tailscale set` must return 1 itself; the prefs re-read below
	# always succeeds and would otherwise turn a failed set into CONFIG OK.
	if [ -n "$endpoints" ]; then
		log "setting relay port ${RELAY_PORT}, static endpoints ${endpoints}"
		if ! tailscale set --relay-server-port="$RELAY_PORT" --relay-server-static-endpoints="$endpoints"; then
			log "tailscale set failed; prefs NOT applied this run"
			return 1
		fi
	else
		log "setting relay port ${RELAY_PORT}; no static endpoint (discovery only)"
		if ! tailscale set --relay-server-port="$RELAY_PORT"; then
			log "tailscale set failed; prefs NOT applied this run"
			return 1
		fi
	fi

	# Re-read prefs so the log proves what actually landed, not what was requested.
	log "prefs: $(tailscale debug prefs 2>/dev/null | grep -i relay | tr -d ' \n' || echo 'unreadable')"
}

selftest() {
	version_ok 1.86.0 || { echo "FAIL 1.86.0"; exit 1; }
	version_ok 1.102.3 || { echo "FAIL 1.102.3 (string compare bug)"; exit 1; }
	version_ok 2.0.0 || { echo "FAIL 2.0.0"; exit 1; }
	! version_ok 1.85.9 || { echo "FAIL 1.85.9 accepted"; exit 1; }
	! version_ok 0.0 || { echo "FAIL 0.0 accepted"; exit 1; }
	[ "$(format_endpoint 192.0.2.2 40000)" = "192.0.2.2:40000" ] || { echo "FAIL v4"; exit 1; }
	[ "$(format_endpoint 2001:db8::1 40000)" = "[2001:db8::1]:40000" ] || { echo "FAIL v6"; exit 1; }
	[ -z "$(format_endpoint '<html>error</html>' 40000)" ] || { echo "FAIL html"; exit 1; }
	[ -z "$(format_endpoint '' 40000)" ] || { echo "FAIL empty"; exit 1; }
	[ -z "$(format_endpoint 'not.an.ip.addr' 40000)" ] || { echo "FAIL letters"; exit 1; }
	RELAY_STATIC_ENDPOINTS="203.0.113.9:41641" RELAY_PORT=40000
	[ "$(static_endpoints)" = "203.0.113.9:41641" ] || { echo "FAIL override"; exit 1; }
	RELAY_STATIC_ENDPOINTS=""
	public_ip() { [ "$1" = 4 ] && echo 192.0.2.2 || echo 2001:db8::1; }
	[ "$(static_endpoints)" = "192.0.2.2:40000,[2001:db8::1]:40000" ] || { echo "FAIL dual-stack: $(static_endpoints)"; exit 1; }
	public_ip() { [ "$1" = 4 ] && echo 192.0.2.2 || echo "<html>"; }
	[ "$(static_endpoints)" = "192.0.2.2:40000" ] || { echo "FAIL v4-only"; exit 1; }
	public_ip() { [ "$1" = 4 ] && echo "" || echo 2001:db8::1; }
	[ "$(static_endpoints)" = "[2001:db8::1]:40000" ] || { echo "FAIL v6-only"; exit 1; }
	public_ip() { echo ""; }
	[ -z "$(static_endpoints)" ] || { echo "FAIL none"; exit 1; }

	# `if configure_relay` disables set -e inside it. A failing `tailscale set`
	# with leftover RelayServerPort=40000 must still log CONFIG FAILED, not OK.
	tsdir=$(mktemp -d)
	printf '%s\n' '#!/bin/sh' \
		'case "$1" in' \
		'version) echo 1.102.3 ;;' \
		'status) printf "{\"BackendState\":\"Running\"}\n" ;;' \
		'set) exit 1 ;;' \
		'debug) printf "{\"RelayServerPort\": 40000}\n" ;;' \
		'*) exit 0 ;;' \
		'esac' >"$tsdir/tailscale"
	chmod +x "$tsdir/tailscale"
	out=$(
		PATH="$tsdir:$PATH"
		RELAY_PORT=40000
		RELAY_WAIT_SECS=2
		RELAY_STATIC_ENDPOINTS=""
		export PATH RELAY_PORT RELAY_WAIT_SECS RELAY_STATIC_ENDPOINTS
		configure_relay_logged 2>&1
	) || true
	rm -rf "$tsdir"
	case "$out" in
	*"RELAY CONFIG FAILED"*) ;;
	*)
		echo "FAIL set-failure must log CONFIG FAILED, got: $out"
		exit 1
		;;
	esac
	case "$out" in
	*"RELAY CONFIG OK"*)
		echo "FAIL set-failure also logged CONFIG OK"
		exit 1
		;;
	esac

	echo "relay-entrypoint selftest OK"
}

# configure_relay runs in the background under `set -e`: without this wrapper an
# aborted subshell would leave no trace and the container would look healthy while
# relaying nothing. These two markers are what install-relay.sh greps for.
configure_relay_logged() {
	if configure_relay; then
		log "RELAY CONFIG OK port=${RELAY_PORT}"
	else
		log "RELAY CONFIG FAILED port=${RELAY_PORT} - prefs not applied, this node is NOT a peer relay"
	fi
}

case "${1:-}" in
selftest) selftest ;;
*)
	configure_relay_logged &
	exec /usr/local/bin/containerboot
	;;
esac
