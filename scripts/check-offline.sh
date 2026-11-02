#!/usr/bin/env bash
# Watch DuckLocal do ordinary local work and fail if it tries to reach anything
# beyond this machine. See docs/network-and-privacy.md.
#
#   scripts/check-offline.sh target/debug/ducklocal
#
# Linux only: it needs strace, and for the window xvfb-run and a Vulkan driver.
# Proves these paths, on this run, and nothing beyond them.
set -euo pipefail

BIN=$(readlink -f "${1:?usage: check-offline.sh PATH_TO_DUCKLOCAL}")
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Nothing the person has set up can change what this sees.
export HOME="$WORK/home" XDG_DATA_HOME="$WORK/home/data" XDG_CONFIG_HOME="$WORK/home/config" XDG_CACHE_HOME="$WORK/home/cache"
mkdir -p "$HOME"
cd "$WORK"

printf 'city,lat,lng,n\nAmsterdam,52.37,4.89,3\nBerlin,52.52,13.40,5\n' >cities.csv
cat >map.dash <<'EOF'
source "cities" {
  path = "cities.csv"
}

query "where" {
  sql = <<SQL
SELECT * FROM cities
SQL
}

plot "map" {
  type  = "map"
  query = query.where
  lat   = "lat"
  lng   = "lng"
}
EOF
cp -r "$ROOT/examples/orders_dashboard" orders

LOG="$WORK/connections.log"

# Every connection any thread tries, and every datagram sent to an address.
watch() {
  strace -f -qq -e trace=connect,sendto,sendmsg -o "$LOG.$1" "${@:2}"
}

# What counts as reaching out, from a set of traces. Inet addresses only: the X
# server and the like are local sockets. Loopback is the window's own listener;
# port 53 is a name lookup, even to a local resolver.
reaching_out() {
  { cat "$@" | grep -E 'AF_INET6?' || true; } |
    { grep -E -v 'inet_addr\("127\.|inet_pton\(AF_INET6, "::1"' || true; }
  { cat "$@" | grep -E 'AF_INET6?' | grep -F 'htons(53)' || true; }
}

# The control: a check that reports nothing has to be one that could report
# something. Try to connect to a reserved address that goes nowhere, under the
# same tracer, and fail unless the filter below catches it.
echo "== control: a real attempt must be caught"
strace -f -qq -e trace=connect,sendto,sendmsg -o "$WORK/control.log" \
  timeout 3 bash -c 'exec 3<>/dev/tcp/192.0.2.1/80' 2>/dev/null || true
if [ -z "$(reaching_out "$WORK/control.log")" ]; then
  echo "FAILED: the control's connection attempt went unseen, so this check proves nothing:" >&2
  cat "$WORK/control.log" >&2
  exit 1
fi
strace -f -qq -e trace=connect,sendto,sendmsg -o "$WORK/loopback.log" \
  timeout 3 bash -c 'exec 3<>/dev/tcp/127.0.0.1/9' 2>/dev/null || true
if [ -n "$(reaching_out "$WORK/loopback.log")" ]; then
  echo "FAILED: the control's loopback attempt was taken for reaching out:" >&2
  cat "$WORK/loopback.log" >&2
  exit 1
fi

echo "== command line"
watch version "$BIN" --version >/dev/null
watch query "$BIN" query --sql "SELECT count(*) AS n FROM 'cities.csv'" >/dev/null
watch export "$BIN" query --sql "SELECT * FROM 'cities.csv' ORDER BY n" --format md >/dev/null
watch schema "$BIN" schema cities.csv >/dev/null
watch stats "$BIN" schema cities.csv --stats >/dev/null
watch check "$BIN" check orders/dashboard.dash >/dev/null
watch check-map "$BIN" check map.dash >/dev/null

echo "== window, on a dashboard and a map, with default settings"
# It stays up until stopped. The endpoint file says it reached its event loop.
xvfb-run -a strace -f -qq -e trace=connect,sendto,sendmsg -o "$LOG.window" \
  timeout -k 5 15 "$BIN" orders/dashboard.dash map.dash >"$WORK/window.out" 2>&1 &
WINDOW=$!
started=
for _ in $(seq 1 30); do
  if find "$XDG_DATA_HOME" -name gui-endpoint.json 2>/dev/null | grep -q .; then
    started=yes
    break
  fi
  sleep 0.5
done
wait "$WINDOW" || true
if [ -z "$started" ]; then
  echo "FAILED: the window did not start, so it was not checked:" >&2
  tail -20 "$WORK/window.out" >&2
  exit 1
fi

echo "== what was tried"
outside=$(reaching_out "$LOG".*)
echo "$(cat "$LOG".* | grep -c -E 'AF_INET6?' || true) inet attempts seen, loopback included"
if [ -n "$outside" ]; then
  echo "FAILED: ordinary local work tried to reach the network:" >&2
  printf '%s\n' "$outside" >&2
  echo "If this is intended, see 'Adding a new outbound path' in docs/network-and-privacy.md" >&2
  exit 1
fi

# A check that saw nothing proves nothing: the tracer must have been watching.
if ! cat "$LOG".window | grep -qE 'connect\('; then
  echo "FAILED: the trace of the window holds no connect() at all, so it saw nothing" >&2
  exit 1
fi
echo "OK: no attempt to reach beyond this machine"
