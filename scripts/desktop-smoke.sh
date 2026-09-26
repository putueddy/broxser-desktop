#!/usr/bin/env bash
# X11 desktop smoke check. It opens the live desktop against the local fixture
# and closes it with Ctrl+Q, then closes a static capture while the fixture holds
# the request. It then kills the desktop with SIGKILL and with SIGINT to its
# process group (Ctrl+C) while live frames stream, and with SIGTERM during a held
# static capture: the browser guardian must stop the browser and remove its
# profile (ADR 0007). Two runs kill the live browser and click Restart twice,
# once followed by Ctrl+Q: at most one browser may start (ADR 0009). The last
# run types while every page animates; use a release build
# (BROXSER_DESKTOP_BIN=target/release/broxser-desktop) for it to cover the GPUI
# atlas race of ADR 0012. Every run must leave no browser process or profile
# behind, and the window must be gone.
# Needs an X11 display (a desktop session or Xvfb), xdotool, python3,
# BROXSER_HELIUM_BIN and a built desktop binary. It does not check rendering
# quality, Wayland, IME, accessibility or a physical GPU.
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
: "${DISPLAY:?set DISPLAY to an X11 display, for example Xvfb :99}"
: "${BROXSER_HELIUM_BIN:?set BROXSER_HELIUM_BIN to the Helium executable}"
command -v xdotool >/dev/null || { echo 'xdotool is required' >&2; exit 1; }
binary=${BROXSER_DESKTOP_BIN:-target/debug/broxser-desktop}
[[ -x $binary ]] || { echo "Build first: cargo build --locked -p broxser-desktop" >&2; exit 1; }

work=$(mktemp -d)
server= watcher=
cleanup() {
  [[ -n $server ]] && kill "$server" 2>/dev/null || true
  [[ -n $watcher ]] && kill "$watcher" 2>/dev/null || true
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

# Browser processes, browser profiles and static preview directories.
leftovers() {
  local processes profiles previews
  processes=$(pgrep -f -- "$TMPDIR/" | wc -l || true)
  profiles=$(find "$TMPDIR" -maxdepth 1 -name 'broxser-cdp-*' | wc -l)
  previews=$(find "$TMPDIR" -maxdepth 1 -name 'broxser-preview-*' | wc -l)
  echo "$processes $profiles $previews"
}

run() {
  local label=$1 wait_for=$2 stop=$3
  shift 3
  local before app window started ended code
  before=$(wc -l < "$work/requests")
  if [[ $stop == INT-group ]]; then
    # Job control gives the app its own process group, as a terminal does, and
    # keeps SIGINT at its default: without it, background jobs ignore SIGINT.
    set -m
    "$binary" --workspace examples/workspace.json "$@" &
    set +m
  else
    "$binary" --workspace examples/workspace.json "$@" &
  fi
  app=$!
  window=$(timeout 20 xdotool search --sync --name '^Broxser$' | head -n 1)
  # Without a window manager GPUI draws its first frame after a configure event.
  xdotool windowsize "$window" 1360 861
  for _ in $(seq 300); do
    [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "$wait_for" || true) -gt 0 ]] && break
    sleep 0.1
  done
  sleep 1
  read -r processes profiles _ < <(leftovers)
  [[ $processes -gt 0 && $profiles -gt 0 ]] || { echo "$label: browser did not start" >&2; exit 1; }
  case $stop in
    quit) xdotool mousemove --window "$window" 600 400 key ctrl+q ;;
    INT-group) kill -INT -- "-$app" ;;
    *) kill "-$stop" "$app" ;;
  esac
  started=$(date +%s%N)
  code=0
  timeout 15 tail -s 0.05 --pid="$app" -f /dev/null || { echo "$label: did not exit" >&2; exit 1; }
  wait "$app" || code=$?
  ended=$(date +%s%N)
  # A killed desktop leaves the cleanup to the browser guardian.
  for _ in $(seq 100); do
    read -r left_processes left_profiles left_previews < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  local cleaned
  cleaned=$(( ($(date +%s%N) - started) / 1000000 ))
  echo "$label: exit $code after $(( (ended - started) / 1000000 )) ms;" \
    "$processes browser processes before, $left_processes after; $left_profiles profiles" \
    "and $left_previews preview directories left; checked $cleaned ms after the stop"
  if xdotool search --name '^Broxser$' >/dev/null 2>&1; then
    echo "$label: a Broxser window is still open" >&2
    return 1
  fi
  [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] || return 1
  case $stop in
    quit) [[ $code -eq 0 && $left_previews -eq 0 ]] ;;
    # Static preview directories belong to the desktop, not to a browser; a
    # killed desktop leaves them for now.
    *) find "$TMPDIR" -maxdepth 1 -name 'broxser-preview-*' -exec rm -rf -- {} + ;;
  esac
}

# Records the name of every browser profile created from now on, one line per
# browser launch, in $1. Returns once it is watching.
watch_profiles() {
  python3 - "$TMPDIR" "$1" <<'PY' &
import os, sys, time
root, out = sys.argv[1], sys.argv[2]
seen = {name for name in os.listdir(root) if name.startswith("broxser-cdp-")}
open(out + ".ready", "w").close()
while True:
    for name in os.listdir(root):
        if name.startswith("broxser-cdp-") and name not in seen:
            seen.add(name)
            with open(out, "a") as log:
                log.write(name + "\n")
    time.sleep(0.002)
PY
  watcher=$!
  for _ in $(seq 100); do [[ -f $1.ready ]] && break; sleep 0.02; done
}

# Kills the owned browser, as a crash would, so that "Restart runtime" appears,
# then clicks it twice without delay: one browser must start (ADR 0009). With
# `quit`, Ctrl+Q follows the clicks at once: at most that browser may start, and
# the window closes once it and the stopped runtime are gone.
restart_run() {
  local label=$1 quit=$2
  local before app window browser launches loaded left_processes left_profiles code=0
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/live.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --name '^Broxser$' | head -n 1)
  xdotool windowsize "$window" 1360 861
  for _ in $(seq 300); do
    [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- /live.html || true) -gt 0 ]] && break
    sleep 0.1
  done
  sleep 1
  # This instance's main browser is the desktop's child naming a private profile.
  browser=$(pgrep -P "$app" -f -- "--user-data-dir=$TMPDIR/broxser-cdp-" || true)
  [[ $(wc -w <<< "$browser") -eq 1 ]] || { echo "$label: expected one browser: $browser" >&2; return 1; }
  kill -KILL "$browser"
  # The runtime reports the stop after removing the profile.
  for _ in $(seq 200); do
    [[ -z $(find "$TMPDIR" -maxdepth 1 -name 'broxser-cdp-*' -print -quit) ]] && break
    sleep 0.05
  done
  sleep 1
  rm -f -- "$work/launches" "$work/launches.ready"
  touch "$work/launches"
  watch_profiles "$work/launches"
  before=$(wc -l < "$work/requests")
  # "Restart runtime" is the last control of the 44 px status bar, 24 px from
  # the right edge of the 1360 × 861 window.
  if [[ $quit == quit ]]; then
    xdotool mousemove --window "$window" 1300 839 click --repeat 2 --delay 0 1 key --delay 0 ctrl+q
  else
    xdotool mousemove --window "$window" 1300 839 click --repeat 2 --delay 0 1
    sleep 3
    loaded=$(tail -n +"$((before + 1))" "$work/requests" | grep -c -- /live.html || true)
    [[ $loaded -gt 0 ]] || { echo "$label: the restarted runtime loaded nothing" >&2; return 1; }
    xdotool mousemove --window "$window" 600 400 key ctrl+q
  fi
  timeout 15 tail -s 0.05 --pid="$app" -f /dev/null || { echo "$label: did not exit" >&2; return 1; }
  wait "$app" || code=$?
  kill "$watcher" 2>/dev/null || true
  wait "$watcher" 2>/dev/null || true
  watcher=
  launches=$(wc -l < "$work/launches")
  read -r left_processes left_profiles _ < <(leftovers)
  echo "$label: $launches browser(s) started by the restart; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  if xdotool search --name '^Broxser$' >/dev/null 2>&1; then
    echo "$label: a Broxser window is still open" >&2
    return 1
  fi
  [[ $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]] || return 1
  if [[ $quit == quit ]]; then [[ $launches -le 1 ]]; else [[ $launches -eq 1 ]]; fi
}

# Succeeds once the window region X Y WIDTH HEIGHT has changed in each of six
# consecutive half seconds, read every 100 ms with XGetImage, within TIMEOUT
# seconds: device frames that are displayed and keep changing. A static page
# changes in a burst of about a second while it loads, then not at all.
# xdotool already needs libX11.
frames_change() {
  python3 - "$@" <<'PY'
import ctypes, sys, time
window = int(sys.argv[1], 0)
x, y, width, height = (int(value) for value in sys.argv[2:6])
deadline = time.monotonic() + float(sys.argv[6])

class XImage(ctypes.Structure):
    _fields_ = [(name, ctypes.c_int) for name in ("width", "height", "xoffset", "format")]
    _fields_ += [("data", ctypes.c_void_p)]
    _fields_ += [(name, ctypes.c_int) for name in ("byte_order", "bitmap_unit",
        "bitmap_bit_order", "bitmap_pad", "depth", "bytes_per_line", "bits_per_pixel")]

xlib = ctypes.CDLL("libX11.so.6")
xlib.XOpenDisplay.argtypes = [ctypes.c_char_p]
xlib.XOpenDisplay.restype = ctypes.c_void_p
xlib.XGetImage.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
                           ctypes.c_uint, ctypes.c_uint, ctypes.c_ulong, ctypes.c_int]
xlib.XGetImage.restype = ctypes.POINTER(XImage)
xlib.XFree.argtypes = [ctypes.c_void_p]
display = xlib.XOpenDisplay(None)
if not display:
    sys.exit("cannot open the X display")
previous, changed, streak = None, False, 0
slice_end = time.monotonic() + 0.5
while time.monotonic() < deadline:
    image = xlib.XGetImage(display, window, x, y, width, height, 0xFFFFFFFF, 2)  # ZPixmap
    if image:
        pixels = ctypes.string_at(image.contents.data,
                                  image.contents.bytes_per_line * image.contents.height)
        xlib.XFree(image.contents.data)
        xlib.XFree(image)
        changed |= previous is not None and pixels != previous
        previous = pixels
    if time.monotonic() >= slice_end:
        streak = streak + 1 if changed else 0
        if streak >= 6:
            sys.exit(0)
        changed, slice_end = False, slice_end + 0.5
    time.sleep(0.1)
sys.exit(1)
PY
}

# Types 1500 keys into the selected device while every device animates, then
# quits. Before the atlas fix in ADR 0012 a release build panicked in GPUI's
# texture atlas within seconds; a debug build rarely reaches that race. Typing
# starts only once the browser requested PAGE and the phone and tablet frames
# are seen animating in the window; the two Guest devices share one browser
# context and may load PAGE from its cache. Every path closes the desktop and
# checks that no browser process or profile is left.
typing_run() {
  local label=$1 page=$2
  local before app window= requests=0 keys failure= left_processes left_profiles code=0
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/$page" &
  app=$!
  window=$(timeout 20 xdotool search --sync --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    xdotool windowsize "$window" 1360 861 || true
    # Without a window manager the window draws and takes keys once the
    # pointer is in it.
    xdotool mousemove --window "$window" 600 400 || true
    for _ in $(seq 300); do
      requests=$(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "/$page" || true)
      [[ $requests -gt 0 ]] && break
      sleep 0.1
    done
    # At 50% zoom in this window the phone frame (195 × 422 px) is at (267, 180)
    # and the tablet frame (384 × 512 px) at (508, 180); the fixture's box moves
    # through these bands of them. The desktop device is below the fold.
    if [[ $requests -eq 0 ]]; then
      failure="the browser did not request /$page within 30 s"
    elif ! frames_change "$window" 270 205 190 75 15; then
      failure="the phone frame did not animate within 15 s"
    elif ! frames_change "$window" 512 215 376 75 15; then
      failure="the tablet frame did not animate within 15 s"
    fi
  fi
  if [[ -z $failure ]]; then
    # Frames churn the atlas for a while first: an unpatched release build
    # then crashed in 4 of 4 runs, against 2 of 4 when typing began at once.
    sleep 20
    keys=$(printf 'x%.0s' $(seq 1500))
    xdotool type --delay 10 "$keys" || failure="xdotool could not type"
    if ! kill -0 "$app" 2>/dev/null; then
      failure="the desktop exited while typing"
    elif [[ -z $failure ]] && ! frames_change "$window" 270 205 190 75 15; then
      failure="the phone frame stopped animating after typing"
    fi
  fi
  if kill -0 "$app" 2>/dev/null; then
    [[ -n $window ]] && xdotool mousemove --window "$window" 600 400 key ctrl+q || true
    if ! timeout 15 tail -s 0.05 --pid="$app" -f /dev/null; then
      failure=${failure:-did not exit within 15 s of Ctrl+Q}
      kill -TERM "$app" 2>/dev/null || true
      timeout 5 tail -s 0.05 --pid="$app" -f /dev/null || kill -KILL "$app" 2>/dev/null || true
    fi
  fi
  wait "$app" || code=$?
  # A desktop that died leaves the browser to its guardian (ADR 0007).
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-1500 keys typed}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

run "live close" "/live.html" quit --url "http://127.0.0.1:$port/live.html"
run "static close during held request" "/hang" quit --static --capture-on-start --url "http://127.0.0.1:$port/hang"
run "live SIGKILL" "/live.html" KILL --url "http://127.0.0.1:$port/live.html"
run "live SIGINT to its process group" "/live.html" INT-group --url "http://127.0.0.1:$port/live.html"
run "static SIGTERM during held request" "/hang" TERM --static --capture-on-start --url "http://127.0.0.1:$port/hang"
restart_run "live Restart clicked twice" stay
restart_run "live Restart clicked twice, then Ctrl+Q" quit
typing_run "typing while pages animate" animation.html
echo "desktop smoke passed"
