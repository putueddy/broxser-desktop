#!/usr/bin/env bash
# X11 desktop smoke check. It opens the live desktop against the local fixture,
# closes it with Ctrl+Q, then closes a static capture while the fixture holds
# the request. Each run must exit 0 and leave no browser process or temporary
# profile behind. Needs an X11 display (a desktop session or Xvfb), xdotool,
# python3, BROXSER_HELIUM_BIN and a built desktop binary. It does not check
# rendering quality, Wayland, IME, accessibility or a physical GPU.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
: "${DISPLAY:?set DISPLAY to an X11 display, for example Xvfb :99}"
: "${BROXSER_HELIUM_BIN:?set BROXSER_HELIUM_BIN to the Helium executable}"
command -v xdotool >/dev/null || { echo 'xdotool is required' >&2; exit 1; }
binary=${BROXSER_DESKTOP_BIN:-target/debug/broxser-desktop}
[[ -x $binary ]] || { echo "Build first: cargo build --locked -p broxser-desktop" >&2; exit 1; }

work=$(mktemp -d)
server=
cleanup() {
  [[ -n $server ]] && kill "$server" 2>/dev/null || true
  rm -rf -- "$work"
}
trap cleanup EXIT
mkdir "$work/tmp"

# Fixture server on a free loopback port. /hang never answers.
python3 - "$work" <<'PY' &
import functools, http.server, os, sys, time
work = sys.argv[1]
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        with open(os.path.join(work, "requests"), "a") as log:
            log.write(self.path + "\n")
        if self.path.startswith("/hang"):
            time.sleep(3600)
        return super().do_GET()
    def log_message(self, *args):
        pass
handler = functools.partial(Handler, directory="examples/fixture")
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
server.daemon_threads = True
with open(os.path.join(work, "port.tmp"), "w") as port:
    port.write(str(server.server_address[1]))
os.rename(os.path.join(work, "port.tmp"), os.path.join(work, "port"))
server.serve_forever()
PY
server=$!
for _ in $(seq 100); do [[ -f $work/port ]] && break; sleep 0.05; done
port=$(cat "$work/port")
touch "$work/requests"

# Profiles and previews of this run live under a private TMPDIR.
export TMPDIR=$work/tmp
unset WAYLAND_DISPLAY

leftovers() {
  local processes profiles
  processes=$(pgrep -f -- "$TMPDIR/" | wc -l || true)
  profiles=$(find "$TMPDIR" -maxdepth 1 -name 'broxser-*' | wc -l)
  echo "$processes $profiles"
}

run() {
  local label=$1 wait_for=$2
  shift 2
  local before app window started ended code
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json "$@" &
  app=$!
  window=$(timeout 20 xdotool search --sync --name '^Broxser$' | head -n 1)
  # Without a window manager GPUI draws its first frame after a configure event.
  xdotool windowsize "$window" 1360 861
  for _ in $(seq 300); do
    [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "$wait_for" || true) -gt 0 ]] && break
    sleep 0.1
  done
  sleep 1
  read -r processes profiles < <(leftovers)
  [[ $processes -gt 0 && $profiles -gt 0 ]] || { echo "$label: browser did not start" >&2; exit 1; }
  xdotool mousemove --window "$window" 600 400 key ctrl+q
  started=$(date +%s%N)
  code=0
  timeout 15 tail -s 0.05 --pid="$app" -f /dev/null || { echo "$label: did not exit" >&2; exit 1; }
  wait "$app" || code=$?
  ended=$(date +%s%N)
  sleep 0.3
  read -r left_processes left_profiles < <(leftovers)
  echo "$label: exit $code after $(( (ended - started) / 1000000 )) ms;" \
    "$processes browser processes before, $left_processes after; $left_profiles temporary directories left"
  [[ $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

run "live close" "/live.html" --url "http://127.0.0.1:$port/live.html"
run "static close during held request" "/hang" --static --capture-on-start --url "http://127.0.0.1:$port/hang"
echo "desktop smoke passed"
