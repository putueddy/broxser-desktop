#!/usr/bin/env bash
# X11 desktop smoke check. It opens the live desktop against the local fixture
# and closes it with Ctrl+Q, then closes a static capture while the fixture holds
# the request. It then kills the desktop with SIGKILL and with SIGINT to its
# process group (Ctrl+C) while live frames stream, and with SIGTERM during a held
# static capture: the browser guardian must stop the browser and remove its
# profile (ADR 0007). Two runs kill the live browser and click Restart twice,
# once followed by Ctrl+Q: at most one runtime may start (ADR 0009), with its
# blank discovery browser followed by the aligned browser (ADR 0019). The last
# run types while every page animates; use a release build
# (BROXSER_DESKTOP_BIN=target/release/broxser-desktop) for it to cover the GPUI
# atlas race of ADR 0012. Every run must leave no browser process or profile
# behind, and the window must be gone. The console run clicks a device's error
# count, clears that device's console in the panel and closes the panel with
# Ctrl+Shift+J (ADR 0023).
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
    def end_headers(self):
        # Every device fetches the page itself, so requests count devices.
        self.send_header("Cache-Control", "no-store")
        super().end_headers()
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

# Profiles and previews of this run live under a private TMPDIR, and the
# application state (recent workspaces, window size; ADR 0022) under a private
# file, so the runs never touch the user's state file.
export TMPDIR=$work/tmp
export BROXSER_STATE_FILE=$work/state.json
unset WAYLAND_DISPLAY

# Without a window manager GPUI draws its first frame after a configure event.
# The desktop reopens at its last size, so one resize to the wanted size can be
# a no-op; the first resize is always a change. Callers wait for a visible
# window before resizing and focusing it.
size_window() {
  xdotool windowsize "$1" 1360 860 || true
  xdotool windowsize "$1" 1360 861 || true
  # Xvfb without a window manager may leave keyboard focus on the root window.
  xdotool windowfocus "$1" || true
}

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
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1)
  size_window "$window"
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
# then clicks it twice without delay: one runtime must start (ADR 0009). The
# pinned Helium gets two sequential private profiles: blank UA discovery and
# the aligned runtime, with the first cleaned up before the second (ADR 0019).
# With `quit`, Ctrl+Q follows at once: at most that pair may start, and the
# window closes once it and the stopped runtime are gone.
restart_run() {
  local label=$1 quit=$2
  local before app window browser launches loaded left_processes left_profiles code=0
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/live.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1)
  size_window "$window"
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
    # Startup can outlast a fixed sleep under load. Require the new runtime's
    # fixture request, within the same bound as the initial startup above.
    for _ in $(seq 300); do
      loaded=$(tail -n +"$((before + 1))" "$work/requests" | grep -c -- /live.html || true)
      [[ $loaded -gt 0 ]] && break
      kill -0 "$app" 2>/dev/null || break
      sleep 0.1
    done
    if [[ $loaded -eq 0 ]]; then
      echo "$label: the restarted runtime loaded nothing within 30 s" >&2
      xdotool mousemove --window "$window" 600 400 key ctrl+q || true
      timeout 15 tail -s 0.05 --pid="$app" -f /dev/null || kill -TERM "$app" 2>/dev/null || true
      wait "$app" || true
      return 1
    fi
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
  if [[ $quit == quit ]]; then [[ $launches -le 2 ]]; else [[ $launches -eq 2 ]]; fi
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
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    size_window "$window"
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

# Prints "X Y X0 Y0 X1 Y1", the centroid and bounding box (window coordinates)
# of the pixels within TOLERANCE per channel of color RRGGBB in the window
# region X Y WIDTH HEIGHT, as soon as at least 20 match within TIMEOUT seconds.
# Fails once TIMEOUT passes without a match. With a ninth argument "absent" it
# instead succeeds, printing nothing, once fewer than 20 pixels match; with
# "filled" it counts only pixels whose neighbor two rows below also matches, so
# horizontal one-pixel borders do not count. Vertical borders still count, so
# keep them outside the region. "absent,filled" combines both.
find_color() {
  python3 - "$@" <<'PY'
import ctypes, sys, time
window = int(sys.argv[1], 0)
x, y, width, height = (int(value) for value in sys.argv[2:6])
wanted = int(sys.argv[6], 16)
tolerance = int(sys.argv[7])
deadline = time.monotonic() + float(sys.argv[8])
modes = set(sys.argv[9].split(",")) if len(sys.argv) > 9 else set()
absent = "absent" in modes
filled = "filled" in modes

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
target = ((wanted >> 16) & 255, (wanted >> 8) & 255, wanted & 255)
while time.monotonic() < deadline:
    image = xlib.XGetImage(display, window, x, y, width, height, 0xFFFFFFFF, 2)  # ZPixmap
    if image:
        stride = image.contents.bytes_per_line
        pixels = ctypes.string_at(image.contents.data, stride * image.contents.height)
        xlib.XFree(image.contents.data)
        xlib.XFree(image)
        def matches(column, row):
            offset = row * stride + column * 4
            b, g, r = pixels[offset], pixels[offset + 1], pixels[offset + 2]
            return abs(r - target[0]) <= tolerance and abs(g - target[1]) <= tolerance \
                and abs(b - target[2]) <= tolerance
        hits = [(column, row) for row in range(height) for column in range(width)
                if matches(column, row) and (not filled or (row + 2 < height and matches(column, row + 2)))]
        if absent and len(hits) < 20:
            sys.exit(0)
        if not absent and len(hits) >= 20:
            xs = [x + column for column, _ in hits]
            ys = [y + row for _, row in hits]
            print(sum(xs) // len(xs), sum(ys) // len(ys), min(xs), min(ys), max(xs), max(ys))
            sys.exit(0)
    time.sleep(0.2)
sys.exit(1)
PY
}

# Opens a page whose only button asks confirm(), clicks it in the phone frame
# and answers the dialog Broxser shows on the device card (ADR 0014): OK, then
# once more Cancel. The page reports each answer to the fixture. Only the
# card's buttons answer a dialog; the run fails if the panel never appears, if
# an answer is not reported, or if the panel stays. Every path closes the
# desktop and checks for leftovers.
dialog_run() {
  local label=$1
  local before app window= failure= panel button code=0 left_processes left_profiles
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/dialog.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    size_window "$window"
    xdotool mousemove --window "$window" 600 400 || true
    for _ in $(seq 300); do
      [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- /dialog.html || true) -gt 0 ]] && break
      sleep 0.1
    done
    # The phone card is the left column; its frame shows the page's blue button.
    for answer in OK Cancel; do
      [[ -n $failure ]] && break
      if ! button=$(find_color "$window" 250 60 230 760 3b82f6 40 30); then
        failure="the phone frame did not show the page ($answer)"
        break
      fi
      read -r x y _ < <(echo "$button")
      xdotool mousemove --window "$window" "$x" "$y" click 1
      # The dialog panel has an orange (WARN) border.
      if ! panel=$(find_color "$window" 250 60 230 760 f2b872 6 10); then
        failure="the dialog panel did not appear ($answer)"
        break
      fi
      read -r _ _ x0 y0 x1 y1 < <(echo "$panel")
      # OK is the accent-colored button, Cancel the raised one, inside the panel.
      if [[ $answer == OK ]]; then
        button=$(find_color "$window" $((x0 + 3)) $((y0 + 3)) $((x1 - x0 - 6)) $((y1 - y0 - 6)) 7ce29b 6 3) || true
      else
        button=$(find_color "$window" $((x0 + 3)) $((y0 + 3)) $((x1 - x0 - 6)) $((y1 - y0 - 6)) 252c29 6 3) || true
      fi
      if [[ -z $button ]]; then
        failure="the $answer button was not found in the panel"
        break
      fi
      read -r x y _ < <(echo "$button")
      xdotool mousemove --window "$window" "$x" "$y" click 1
      local expected
      expected=$([[ $answer == OK ]] && echo true || echo false)
      for _ in $(seq 100); do
        [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "/event?dialog=$expected" || true) -gt 0 ]] && break
        sleep 0.1
      done
      if [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "/event?dialog=$expected" || true) -eq 0 ]]; then
        failure="the page did not report confirm=$expected within 10 s"
        break
      fi
      # The card repaints once the runtime reports the dialog closed.
      if ! find_color "$window" 250 60 230 760 f2b872 6 5 absent; then
        failure="the dialog panel stayed after $answer"
        break
      fi
    done
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
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-OK and Cancel answered through the card}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

# Opens a page whose only button calls window.open(), clicks it in the phone
# frame and checks that the window never runs (ADR 0015): the target page
# reports "popup" to the fixture while it runs as a window. The card then shows
# the closed window with "Open here"; clicking it loads the page in the phone
# itself, which reports "page". Every path closes the desktop and checks for
# leftovers.
popup_run() {
  local label=$1
  local before app window= failure= button code=0 left_processes left_profiles
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/popup.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    size_window "$window"
    xdotool mousemove --window "$window" 600 400 || true
    for _ in $(seq 300); do
      [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- /popup.html || true) -gt 0 ]] && break
      sleep 0.1
    done
    if ! button=$(find_color "$window" 250 60 230 760 3b82f6 40 30); then
      failure="the phone frame did not show the page"
    else
      read -r x y x0 y0 x1 y1 < <(echo "$button")
      xdotool mousemove --window "$window" "$x" "$y" click 1
      # The notice's "Open here" button is accent-filled, below the frame; the
      # selected card's one-pixel border has the same color.
      if ! button=$(find_color "$window" 262 $((y1 + 1)) 206 $((820 - y1 - 1)) 7ce29b 6 10 filled); then
        failure="the card did not report the closed window"
      else
        sleep 2
        if [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "popup-alive=popup" || true) -gt 0 ]]; then
          failure="the window ran although Broxser closed it"
        else
          read -r x y _ < <(echo "$button")
          xdotool mousemove --window "$window" "$x" "$y" click 1
          for _ in $(seq 100); do
            [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "popup-alive=page" || true) -gt 0 ]] && break
            sleep 0.1
          done
          if [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "popup-alive=page" || true) -eq 0 ]]; then
            failure="Open here did not load the page in the phone within 10 s"
          elif ! find_color "$window" 250 60 230 760 16a34a 40 10 >/dev/null; then
            failure="the phone frame did not show the opened page"
          elif ! find_color "$window" 262 $((y1 + 1)) 206 $((820 - y1 - 1)) 7ce29b 6 5 absent,filled; then
            failure="the report stayed after Open here"
          fi
        fi
      fi
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
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-closed at once, opened here on request}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

download_run() {
  local label=$1
  local before app window= failure= button code=0 left_processes left_profiles
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/download.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    # Tall enough for the report below the phone frame.
    xdotool windowsize "$window" 1360 1000 || true
    xdotool mousemove --window "$window" 600 400 || true
    for _ in $(seq 300); do
      [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- /download.html || true) -gt 0 ]] && break
      sleep 0.1
    done
    if ! button=$(find_color "$window" 250 60 230 760 3b82f6 40 30); then
      failure="the phone frame did not show the page"
    else
      read -r x y x0 y0 x1 y1 < <(echo "$button")
      xdotool mousemove --window "$window" "$x" "$y" click 1
      for _ in $(seq 100); do
        [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "download.html?file" || true) -gt 0 ]] && break
        sleep 0.1
      done
      # The report's Dismiss button is accent-filled, below the frame; the
      # selected card's one-pixel border has the same color.
      if [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- "download.html?file" || true) -eq 0 ]]; then
        failure="the browser did not request the file within 10 s"
      elif ! button=$(find_color "$window" 262 $((y1 + 1)) 206 $((960 - y1 - 1)) 7ce29b 6 10 filled); then
        failure="the card did not report the refused download"
      elif ! find_color "$window" 250 60 230 760 3b82f6 40 3 >/dev/null; then
        failure="the phone left its page"
      else
        read -r x y _ < <(echo "$button")
        xdotool mousemove --window "$window" "$x" "$y" click 1
        if ! find_color "$window" 262 $((y1 + 1)) 206 $((960 - y1 - 1)) 7ce29b 6 5 absent,filled; then
          failure="the report stayed after Dismiss"
        fi
      fi
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
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  # No file may appear anywhere the browser could write.
  local saved
  local download_roots=("$TMPDIR")
  [[ -d $HOME/Downloads ]] && download_roots+=("$HOME/Downloads")
  saved=$(find "${download_roots[@]}" -name 'notes.txt*' | wc -l)
  [[ $saved -eq 0 ]] || failure=${failure:-"$saved file(s) named notes.txt saved"}
  echo "$label: ${failure:-refused and reported, nothing saved}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

# A 1440 CSS-pixel desktop at 100% is wider than this 1360-pixel window.
# Dismiss must remain reachable after scrolling down the host canvas. After
# dismissing, kill only this app's browser and explicitly restart: the first
# new download must have its own visible report, as must another download
# after dismissing that one. This covers both report layout and runtime-local
# dismissal counts (ADR 0016), which the small phone smoke cannot exercise.
download_desktop_restart_run() {
  local label=$1
  local app window= failure= button browser stage before clicks x y code=0
  local left_processes left_profiles
  python3 - "$work/download-desktop.json" <<'PY'
import json, sys
with open("examples/workspace.json") as source:
    workspace = json.load(source)
workspace["devices"] = [device for device in workspace["devices"] if device["id"] == "desktop"]
workspace["devices"][0].update(width=1440, height=900)
with open(sys.argv[1], "w") as destination:
    json.dump(workspace, destination)
PY
  "$binary" --workspace "$work/download-desktop.json" --url "http://127.0.0.1:$port/download.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    # Xvfb has no window manager; focus before resize to get the first frame.
    for _ in $(seq 100); do
      xdotool windowfocus "$window" 2>/dev/null && break
      sleep 0.05
    done
    size_window "$window"
    xdotool mousemove --window "$window" 600 400 || true
    if ! button=$(find_color "$window" 1300 10 40 38 252c29 6 30 filled); then
      failure="the zoom-in control did not appear"
    else
      read -r x y _ <<< "$button"
      # Four steps of 12.5% take the initial 50% display scale to 100%.
      xdotool mousemove --window "$window" "$x" "$y" click --repeat 4 --delay 250 1
    fi
    for stage in initial after-restart fresh; do
      [[ -n $failure ]] && break
      # The gutter belongs to the host canvas, so wheel events scroll the
      # card rather than the page. Scroll to the frame before each gesture.
      xdotool mousemove --window "$window" 242 400 click --repeat 30 --delay 20 4
      if ! button=$(find_color "$window" 267 180 1080 600 3b82f6 40 30); then
        failure="$stage: the desktop frame did not show the page"
        break
      fi
      before=$(grep -c -- '/event?download=clicked' "$work/requests" || true)
      read -r x y _ <<< "$button"
      xdotool mousemove --window "$window" "$x" "$y" click 1
      for _ in $(seq 100); do
        clicks=$(grep -c -- '/event?download=clicked' "$work/requests" || true)
        [[ $clicks -gt $before ]] && break
        sleep 0.1
      done
      if [[ $clicks -le $before ]]; then
        failure="$stage: the page did not report the download gesture within 10 s"
        break
      fi
      xdotool mousemove --window "$window" 242 400 click --repeat 30 --delay 20 5
      # Interior only: filled filtering also matches vertical card borders.
      # The desktop's right border is beyond the window at 100%; x267 also
      # excludes its left border. The status bar starts below this region.
      if ! button=$(find_color "$window" 267 60 1080 750 7ce29b 6 10 filled); then
        failure="$stage: Dismiss was not visible in the desktop card at 100%"
        break
      fi
      read -r x y _ <<< "$button"
      xdotool mousemove --window "$window" "$x" "$y" click 1
      if ! find_color "$window" 267 60 1080 750 7ce29b 6 5 absent,filled; then
        failure="$stage: the report stayed after Dismiss"
        break
      fi
      echo "$label: $stage report visible and dismissed at 100%"
      if [[ $stage == initial ]]; then
        browser=$(pgrep -P "$app" -f -- "--user-data-dir=$TMPDIR/broxser-cdp-" || true)
        if [[ $(wc -w <<< "$browser") -ne 1 ]]; then
          failure="expected one app-owned browser: $browser"
          break
        fi
        kill -KILL "$browser"
        for _ in $(seq 200); do
          [[ -z $(find "$TMPDIR" -maxdepth 1 -name 'broxser-cdp-*' -print -quit) ]] && break
          sleep 0.05
        done
        if ! button=$(find_color "$window" 1100 818 245 36 7ce29b 6 10 filled); then
          failure="Restart runtime did not appear after the browser stopped"
          break
        fi
        before=$(wc -l < "$work/requests")
        read -r x y _ <<< "$button"
        xdotool mousemove --window "$window" "$x" "$y" click 1
        for _ in $(seq 200); do
          [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- '/download.html' || true) -gt 0 ]] && break
          sleep 0.1
        done
        if [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- '/download.html' || true) -eq 0 ]]; then
          failure="the restarted browser did not load the fixture within 20 s"
          break
        fi
      fi
    done
  fi
  if kill -0 "$app" 2>/dev/null; then
    [[ -n $window ]] && xdotool windowfocus "$window" mousemove --window "$window" 600 400 key ctrl+q || true
    if ! timeout 15 tail -s 0.05 --pid="$app" -f /dev/null; then
      failure=${failure:-did not exit within 15 s of Ctrl+Q}
      kill -TERM "$app" 2>/dev/null || true
      timeout 5 tail -s 0.05 --pid="$app" -f /dev/null || kill -KILL "$app" 2>/dev/null || true
    fi
  fi
  wait "$app" || code=$?
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-three independent reports dismissed, including the first after restart}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  if xdotool search --name '^Broxser$' >/dev/null 2>&1; then
    echo "$label: a Broxser window is still open" >&2
    return 1
  fi
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

# Touch cancellation must cross the real canvas handlers: a release outside
# its bounds or focus moving to the URL bar must not activate the start point.
# The final normal tap reports the complete event history, including any
# unwanted right/middle click or stale hover after the canceled gesture.
touch_run() {
  local label=$1
  local before app window= failure= button x y code=0 left_processes left_profiles
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/touch.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    size_window "$window"
    if ! button=$(find_color "$window" 250 60 230 760 3b82f6 40 30); then
      failure="the phone frame did not show the touch target"
    else
      read -r x y _ < <(echo "$button")
      xdotool mousemove --window "$window" "$x" "$y" click 3 click 2 \
        mousedown 1 mousemove --window "$window" 200 "$y" mouseup 1
      for _ in $(seq 100); do
        [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- 'touch=touchcancel&w=390' || true) -ge 1 ]] && break
        sleep 0.1
      done
      xdotool mousemove --window "$window" "$x" "$y" mousedown 1 key ctrl+l mouseup 1 key Escape
      for _ in $(seq 100); do
        [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- 'touch=touchcancel&w=390' || true) -ge 2 ]] && break
        sleep 0.1
      done
      xdotool mousemove --window "$window" $((x + 10)) "$y" click 1
      for _ in $(seq 100); do
        [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- 'touch=click&w=390' || true) -ge 1 ]] && break
        sleep 0.1
      done
      if ! python3 - "$work/requests" "$before" <<'PY'
import sys, urllib.parse
reports = []
for path in open(sys.argv[1]).read().splitlines()[int(sys.argv[2]):]:
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(path).query)
    if query.get('w') == ['390'] and 'events' in query:
        reports.append(query['events'][0].split(','))
actual = max(reports, key=len, default=[])
expected = ['touchstart', 'touchcancel', 'touchstart', 'touchcancel',
            'touchstart', 'touchend', 'click']
if actual != expected:
    sys.exit('unexpected touch history: ' + repr(actual))
PY
      then
        failure="outside release, focus loss or unsupported buttons changed the touch history"
      fi
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
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-outside release and focus loss canceled, next tap clicked}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

# Opens the workspace panel with Ctrl+Shift+W on a copy of the example
# workspace, adds the first preset device to the draft and applies it (a
# restart with four devices), removes the first device and applies again
# (three devices), and saves the draft to the copy. An invalid URL must leave
# that file byte-for-byte unchanged; a valid URL is then saved immediately
# before Ctrl+Q, and the save must finish before exit. The panel edits a draft:
# the running workspace changes only on Apply (ADR 0022). Afterwards the copy
# must hold the preset device and not the removed one, and the run's state file
# must name the copy and the window size and hold no page address.
workspace_run() {
  local label=$1
  local before mark app window= failure= found code=0 left_processes left_profiles close_requested=0
  local workspace=$work/workspace.json state=$work/state.json
  local snapshot=$work/workspace-before-invalid.json
  local saved_url="http://127.0.0.1:$port/live.html?workspace-save=close"
  cp examples/workspace.json "$workspace"
  before=$(wc -l < "$work/requests")
  BROXSER_STATE_FILE=$state "$binary" --workspace "$workspace" --url "http://127.0.0.1:$port/live.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  # Requests of /live.html since line $1 of the log reach $2 within 30 s.
  loaded() {
    for _ in $(seq 300); do
      [[ $(tail -n +"$(($1 + 1))" "$work/requests" | grep -c -- '/live.html' || true) -ge $2 ]] && return 0
      sleep 0.1
    done
    return 1
  }
  # The panel's buttons are filled: Add, Apply and Save in the accent color,
  # Remove in the danger color. Buttons of one kind stack vertically at the
  # right edge, so the first one sits at the top right of the bounding box of
  # that color, and the lowest accent button is Apply or Save, at the left.
  # Regions of the 340 px panel next to the 230 px sidebar: its right edge
  # holds the Add and Remove columns (the selected card's accent border sits
  # left of it while the panel is still closed); its left part holds the one
  # accent action at the top: Apply while the draft differs, else Save.
  panel_button() {
    find_color "$window" "$2" 60 "$3" 780 "$1" 8 10 filled
  }
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  elif ! loaded "$before" 3; then
    failure="the three devices did not load the page"
  else
    size_window "$window"
    xdotool mousemove --window "$window" 600 400 key ctrl+shift+w
    if ! found=$(panel_button 7ce29b 500 70); then
      failure="the panel did not open with Add buttons"
    else
      # Add buttons are right-aligned; Save shares the color at the bottom left.
      read -r _ _ _ y0 x1 _ < <(echo "$found")
      xdotool mousemove --window "$window" $((x1 - 15)) $((y0 + 8)) click 1
      sleep 0.5
      mark=$(wc -l < "$work/requests")
      read -r x y _ < <(panel_button 7ce29b 232 170)
      xdotool mousemove --window "$window" "$x" "$y" click 1
      if ! loaded "$mark" 4; then
        failure="Apply did not restart with four devices"
      elif ! found=$(panel_button e07a7a 480 90); then
        failure="the draft showed no Remove button"
      else
        read -r _ _ _ y0 x1 _ < <(echo "$found")
        xdotool mousemove --window "$window" $((x1 - 15)) $((y0 + 8)) click 1
        sleep 0.5
        mark=$(wc -l < "$work/requests")
        read -r x y _ < <(panel_button 7ce29b 232 170)
        xdotool mousemove --window "$window" "$x" "$y" click 1
        if ! loaded "$mark" 3; then
          failure="Apply did not restart with three devices"
        else
          sleep 1
          read -r x y _ < <(panel_button 7ce29b 232 170)
          xdotool mousemove --window "$window" "$x" "$y" click 1
          for _ in $(seq 50); do
            grep -q '"small-phone"' "$workspace" && break
            sleep 0.1
          done
          if ! grep -q '"small-phone"' "$workspace"; then
            failure="Save did not write the preset device"
          elif grep -q '"id": "phone"' "$workspace"; then
            failure="Save kept the removed device"
          else
            # Keep valid JSON with distinct bytes, so an invalid Save that
            # rewrites the old draft is caught even when its contents match.
            printf '\n' >> "$workspace"
            cp "$workspace" "$snapshot"
            xdotool key --clearmodifiers ctrl+l
            xdotool type --clearmodifiers --delay 0 'file:///tmp/broxser-invalid-save'
            # Locate Save again after each notice, rather than retaining its
            # coordinates from the previous action.
            if ! found=$(panel_button 7ce29b 232 170); then
              failure="Save was unavailable for the invalid URL"
            else
              read -r x y _ < <(echo "$found")
              xdotool mousemove --window "$window" "$x" "$y" click 1
              sleep 0.5
              if ! cmp -s "$snapshot" "$workspace"; then
                failure="Save changed the file despite the invalid URL"
              else
                xdotool key --clearmodifiers ctrl+l
                xdotool type --clearmodifiers --delay 0 "$saved_url"
                if ! found=$(panel_button 7ce29b 232 170); then
                  failure="Save was unavailable after the invalid URL notice"
                else
                  read -r x y _ < <(echo "$found")
                  xdotool mousemove --window "$window" "$x" "$y" click 1 key --delay 0 ctrl+q
                  close_requested=1
                fi
              fi
            fi
          fi
        fi
      fi
    fi
  fi
  if kill -0 "$app" 2>/dev/null; then
    if [[ $close_requested -eq 0 ]]; then
      [[ -n $window ]] && xdotool mousemove --window "$window" 600 400 key ctrl+q || true
    fi
    if ! timeout 15 tail -s 0.05 --pid="$app" -f /dev/null; then
      failure=${failure:-did not exit within 15 s of Ctrl+Q}
      kill -TERM "$app" 2>/dev/null || true
      timeout 5 tail -s 0.05 --pid="$app" -f /dev/null || kill -KILL "$app" 2>/dev/null || true
    fi
  fi
  wait "$app" || code=$?
  if [[ -z $failure ]] && ! python3 - "$workspace" "$saved_url" <<'PY'
import json, sys
with open(sys.argv[1]) as source:
    workspace = json.load(source)
sys.exit(0 if workspace["url"] == sys.argv[2] else 1)
PY
  then
    failure="Save followed by Ctrl+Q did not persist the requested URL"
  fi
  if [[ -z $failure ]]; then
    if [[ ! -f $state ]]; then
      failure="no state file was written"
    elif ! grep -q '"window"' "$state" || ! grep -q 'workspace.json' "$state"; then
      failure="the state file lacks the window size or the workspace path"
    elif grep -q 'http' "$state"; then
      failure="the state file holds a page address"
    fi
  fi
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-draft saved; invalid URL rejected; valid URL saved before close; state kept}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  if xdotool search --name '^Broxser$' >/dev/null 2>&1; then
    echo "$label: a Broxser window is still open" >&2
    return 1
  fi
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

# Three devices side by side; the panel removes the first and Apply restarts
# without it. At once the pointer moves over, scrolls and clicks the third
# frame, then clicks Hide on the sidebar's third row: GPUI delivers these to
# the listeners of the frame drawn before Apply, which still name index 2 of a
# two-device list (ADR 0022). xdotool's click pauses after the release, so the
# button is pressed and released separately to arrive before the redraw. The
# desktop must restart with two devices and close cleanly.
apply_input_run() {
  local label=$1
  local before mark app window= failure= found code=0 left_processes left_profiles
  local x y x0 x1 y0 apply_x apply_y
  local workspace=$work/apply-input.json state=$work/apply-input-state.json
  python3 - "$workspace" <<'PY'
import json, sys
device = {"width": 360, "height": 640, "device_scale_factor": 1.0,
          "mobile": False, "touch": False, "session": "guest"}
workspace = {
    "schema_version": 1,
    "name": "Three small",
    "url": "http://127.0.0.1:4173",
    "sessions": [{"id": "guest", "name": "Guest"}],
    "devices": [dict(device, id=name.lower(), name=name) for name in "ABC"],
}
with open(sys.argv[1], "w") as destination:
    json.dump(workspace, destination)
PY
  before=$(wc -l < "$work/requests")
  BROXSER_STATE_FILE=$state "$binary" --workspace "$workspace" --url "http://127.0.0.1:$port/live.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --onlyvisible --name '^Broxser$' | head -n 1) || true
  loaded() {
    for _ in $(seq 300); do
      [[ $(tail -n +"$(($1 + 1))" "$work/requests" | grep -c -- '/live.html' || true) -ge $2 ]] && return 0
      kill -0 "$app" 2>/dev/null || return 1
      sleep 0.1
    done
    return 1
  }
  panel_button() {
    find_color "$window" "$2" 60 "$3" 780 "$1" 8 10 filled
  }
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  elif ! loaded "$before" 3; then
    failure="the three devices did not load the page"
  else
    size_window "$window"
    xdotool mousemove --window "$window" 600 400 key ctrl+shift+w
    # Until the panel is drawn, a card's error count lies where its Remove
    # column will be and has the same danger color; wait for the Add column.
    if ! panel_button 7ce29b 500 70 >/dev/null; then
      failure="the panel did not open with Add buttons"
    elif ! found=$(panel_button e07a7a 480 90); then
      failure="the panel showed no Remove button"
    else
      read -r _ _ _ y0 x1 _ < <(echo "$found")
      xdotool mousemove --window "$window" $((x1 - 15)) $((y0 + 8)) click 1
      sleep 0.5
      # The pages' dark headers and buttons lie right of the panel; once all
      # three frames show them (about 620 px at 50%), the right edge of their
      # bounding box is in the third frame.
      for _ in $(seq 20); do
        found=$(find_color "$window" 580 60 770 700 17312b 10 1 filled) || found=
        read -r _ _ x0 y0 x1 _ < <(echo "${found:-0 0 0 0 0 0}")
        ((x1 - x0 >= 500)) && break
        found=
        sleep 0.5
      done
      if [[ -z $found ]]; then
        failure="the three frames did not show the page"
      else
        x=$((x1 - 40)) y=$((y0 + 150))
        if ! found=$(panel_button 7ce29b 232 170); then
          failure="Apply was unavailable after Remove"
        else
          read -r apply_x apply_y _ < <(echo "$found")
          mark=$(wc -l < "$work/requests")
          # Apply, then move, scroll and click in the third frame, then click
          # Hide on the sidebar's third row, outside every frame.
          xdotool mousemove --window "$window" "$apply_x" "$apply_y" mousedown 1 mouseup 1 \
            mousemove --window "$window" "$x" "$y" mousemove --window "$window" $((x + 4)) "$y" \
            mousedown 5 mouseup 5 mousedown 1 mouseup 1 \
            mousemove --window "$window" 195 337 mousedown 1 mouseup 1
          if ! loaded "$mark" 2; then
            failure="the desktop did not restart with two devices"
          fi
        fi
      fi
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
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-restarted with two devices, then closed}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  if xdotool search --name '^Broxser$' >/dev/null 2>&1; then
    echo "$label: a Broxser window is still open" >&2
    return 1
  fi
  [[ -z $failure && $code -eq 0 && $left_processes -eq 0 && $left_profiles -eq 0 ]]
}

# Every device's page logs an error and a warning (ADR 0023). The phone's card
# shows a filled count; clicking it opens the Console panel for the phone,
# where Clear empties the phone's console only; Ctrl+Shift+J closes the panel.
# Regions at 50%: the phone card spans x 254-474 without the panel and moves
# 340 px right with it; the panel's Clear sits at its top right (x 480-570),
# where nothing else is filled in the accent color.
console_run() {
  local label=$1
  local before app window= failure= found x0 y0 x y code=0 left_processes left_profiles
  before=$(wc -l < "$work/requests")
  "$binary" --workspace examples/workspace.json --url "http://127.0.0.1:$port/console.html" &
  app=$!
  window=$(timeout 20 xdotool search --sync --name '^Broxser$' | head -n 1) || true
  if [[ -z $window ]]; then
    failure="no window within 20 s"
  else
    size_window "$window"
    xdotool mousemove --window "$window" 900 400 || true
    for _ in $(seq 300); do
      [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- 'console.html?logged' || true) -ge 3 ]] && break
      sleep 0.1
    done
    if [[ $(tail -n +"$((before + 1))" "$work/requests" | grep -c -- 'console.html?logged' || true) -lt 3 ]]; then
      failure="the pages did not log within 30 s"
    elif ! found=$(find_color "$window" 250 60 230 760 e07a7a 8 15 filled); then
      failure="the phone card showed no console count"
    else
      read -r _ _ x0 y0 _ _ <<< "$found"
      xdotool mousemove --window "$window" $((x0 + 10)) $((y0 + 5)) click 1
      if ! found=$(find_color "$window" 480 60 90 80 7ce29b 8 10 filled); then
        failure="the count did not open the Console panel"
      else
        read -r x y _ <<< "$found"
        xdotool mousemove --window "$window" "$x" "$y" click 1
        if ! find_color "$window" 590 60 230 760 e07a7a 8 10 absent,filled >/dev/null; then
          failure="Clear left the phone's count"
        elif ! find_color "$window" 840 60 400 760 e07a7a 8 5 filled >/dev/null; then
          failure="Clear removed the tablet's count too"
        else
          xdotool mousemove --window "$window" 900 400 key ctrl+shift+j
          if ! find_color "$window" 480 60 90 80 7ce29b 8 10 absent,filled >/dev/null; then
            failure="Ctrl+Shift+J did not close the panel"
          elif ! find_color "$window" 250 60 230 760 3b82f6 40 10 >/dev/null; then
            failure="the phone frame did not return after the panel closed"
          fi
        fi
      fi
    fi
  fi
  if kill -0 "$app" 2>/dev/null; then
    [[ -n $window ]] && xdotool mousemove --window "$window" 900 400 key ctrl+q || true
    if ! timeout 15 tail -s 0.05 --pid="$app" -f /dev/null; then
      failure=${failure:-did not exit within 15 s of Ctrl+Q}
      kill -TERM "$app" 2>/dev/null || true
      timeout 5 tail -s 0.05 --pid="$app" -f /dev/null || kill -KILL "$app" 2>/dev/null || true
    fi
  fi
  wait "$app" || code=$?
  for _ in $(seq 100); do
    read -r left_processes left_profiles _ < <(leftovers)
    [[ $left_processes -eq 0 && $left_profiles -eq 0 ]] && break
    sleep 0.05
  done
  echo "$label: ${failure:-counted, opened, cleared for one device and closed}; exit $code;" \
    "$left_processes browser processes and $left_profiles profiles left"
  if xdotool search --name '^Broxser$' >/dev/null 2>&1; then
    echo "$label: a Broxser window is still open" >&2
    return 1
  fi
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
dialog_run "dialog answered on the card"
popup_run "popup closed and opened on the card"
download_run "download refused on the card"
download_desktop_restart_run "desktop download dismissal after restart"
touch_run "touch cancellation through the canvas"
workspace_run "workspace panel edits a draft and saves it"
apply_input_run "input right after Apply removed a device"
console_run "console panel counts and clears one device"
echo "desktop smoke passed"
