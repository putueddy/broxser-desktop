#!/usr/bin/env python3
"""Real Fcitx5 Pinyin / XIM probe against the Broxser desktop.

Run with --desktop and --browser pointing to explicit executable paths. The
seven Arch package archives (including Vulkan's Lavapipe software driver) go
in BROXSER_IME_PACKAGES (default:
/tmp/broxser-ime-runtime/packages); each is checked against extra.db before
extraction into the selected temporary runtime. Configuration and browser
profiles stay in the output directory; nothing is installed into the system.

The default probe clicks the first fixture input, types nihao + Space, and
requires a Chinese commit and DOM composition events. Use --manual-seconds 120
to inspect all fields, candidate location, cancellation, scroll and device
switching yourself. The event log and screenshot remain in the output directory.
"""

import argparse
import ctypes
import functools
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import threading
import time


REPO = Path(__file__).resolve().parent.parent
PACKAGES = (
    "libime-1.1.16-1",
    "marisa-0.3.1-1",
    "opencc-1.3.2-1",
    "fcitx5-chinese-addons-5.1.14-1",
    "xorg-server-xvfb-21.1.24-1",
    "xdotool-4.20260303.1-1",
    "vulkan-swrast-1:26.2.2-1",
)


def checked_runtime(packages: Path, runtime: Path, database: Path) -> None:
    """Verify every archive against the local package database before unpacking."""
    archives = []
    for name in PACKAGES:
        archive = packages / f"{name}-x86_64.pkg.tar.zst"
        if not archive.is_file():
            raise RuntimeError(f"missing package: {archive}")
        desc = subprocess.check_output(
            ["bsdtar", "-xOf", str(database), f"{name}/desc"], text=True
        )
        expected = desc.split("%SHA256SUM%\n", 1)[1].splitlines()[0]
        with archive.open("rb") as source:
            actual = hashlib.file_digest(source, "sha256").hexdigest()
        if actual != expected:
            raise RuntimeError(f"SHA256 mismatch: {archive}: {actual} != {expected}")
        print(f"verified {archive.name}: {actual}", flush=True)
        archives.append(archive)
    if not (runtime / "usr/lib/fcitx5/libpinyin.so").exists() or not (runtime / "usr/share/vulkan/icd.d/lvp_icd.json").exists():
        runtime.mkdir(parents=True, exist_ok=True)
        for archive in archives:
            subprocess.run(["bsdtar", "-xf", str(archive), "-C", str(runtime)], check=True)


def isolated_env(output: Path, runtime: Path) -> dict[str, str]:
    for leaf in ("config/fcitx5/conf", "cache", "data", "tmp"):
        (output / leaf).mkdir(parents=True, exist_ok=True)
    (output / "config/fcitx5/profile").write_text(
        "[Groups/0]\nName=Default\nDefault Layout=us\nDefaultIM=pinyin\n\n"
        "[Groups/0/Items/0]\nName=keyboard-us\n\n"
        "[Groups/0/Items/1]\nName=pinyin\n\n[GroupOrder]\n0=Default\n"
    )
    (output / "config/fcitx5/conf/xim.conf").write_text("UseOnTheSpot=True\n")
    env = os.environ.copy()
    env.update(
        XDG_CONFIG_HOME=str(output / "config"),
        XDG_CACHE_HOME=str(output / "cache"),
        XDG_DATA_HOME=str(output / "data"),
        XDG_DATA_DIRS=f"{runtime}/usr/share:/usr/share",
        FCITX_ADDON_DIRS=f"{runtime}/usr/lib/fcitx5:/usr/lib/fcitx5",
        FCITX_DATA_DIRS=f"{runtime}/usr/share/fcitx5:/usr/share/fcitx5",
        FCITX_CONFIG_DIRS=f"{runtime}/usr/share/fcitx5:/usr/share/fcitx5",
        LD_LIBRARY_PATH=f"{runtime}/usr/lib" + (f":{env['LD_LIBRARY_PATH']}" if env.get("LD_LIBRARY_PATH") else ""),
        PATH=f"{runtime}/usr/bin:{env['PATH']}",
        TMPDIR=str(output / "tmp"),
        XMODIFIERS="@im=fcitx",
        GTK_IM_MODULE="fcitx",
        QT_IM_MODULE="fcitx",
        LC_ALL="en_US.UTF-8",
        XDG_SESSION_TYPE="x11",
        GDK_BACKEND="x11",
        QT_QPA_PLATFORM="xcb",
        VK_DRIVER_FILES=str(runtime / "usr/share/vulkan/icd.d/lvp_icd.json"),
        VK_ICD_FILENAMES=str(runtime / "usr/share/vulkan/icd.d/lvp_icd.json"),
        LIBGL_ALWAYS_SOFTWARE="1",
    )
    env.pop("WAYLAND_DISPLAY", None)
    return env


class Events(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, directory: str, event_file: Path, request_file: Path, **kwargs):
        self.event_file = event_file
        self.request_file = request_file
        super().__init__(*args, directory=directory, **kwargs)

    def do_GET(self):
        with self.request_file.open("a") as log:
            log.write(self.path + "\n")
        super().do_GET()

    def do_POST(self):
        if self.path != "/event":
            self.send_error(404)
            return
        size = int(self.headers.get("Content-Length", "0"))
        if size > 8192:
            self.send_error(413)
            return
        try:
            event = json.loads(self.rfile.read(size))
            if not isinstance(event, dict) or "type" not in event:
                raise ValueError("invalid event")
        except (ValueError, UnicodeDecodeError):
            self.send_error(400)
            return
        with self.event_file.open("a") as log:
            log.write(json.dumps(event, ensure_ascii=False) + "\n")
        self.send_response(204)
        self.end_headers()

    def log_message(self, *_):
        pass


def wait_until(predicate, seconds: float, message: str):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(message)


def events(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def capture_owned_window(path: Path, pid: int, xid: str, env: dict[str, str]) -> bool:
    """Capture only our window, never the whole X/Wayland desktop."""
    if shutil.which("hyprctl") and shutil.which("grim"):
        result = subprocess.run(["hyprctl", "-j", "clients"], capture_output=True, text=True)
        if result.returncode == 0:
            own = next((client for client in json.loads(result.stdout)
                        if client.get("pid") == pid and client.get("title") == "Broxser"), None)
            if own:
                x, y = own["at"]
                width, height = own["size"]
                subprocess.run(["grim", "-g", f"{x},{y} {width}x{height}", str(path)],
                               timeout=10, capture_output=True)
    if not path.exists() and shutil.which("import"):
        subprocess.run(["import", "-display", env["DISPLAY"], "-window", xid, str(path)],
                       env=env, timeout=10, capture_output=True)
    return path.exists()


def capture_private_display(path: Path, env: dict[str, str]) -> bool:
    """The caller must assert this DISPLAY belongs exclusively to this test."""
    if shutil.which("import"):
        subprocess.run(["import", "-display", env["DISPLAY"], "-window", "root", str(path)],
                       env=env, timeout=10, capture_output=True)
    return path.exists()


def latest_value(path: Path, page_id: str, target: str) -> str:
    matches = [e for e in events(path) if e.get("pageId") == page_id
               and e.get("target") == target and e.get("type") == "input"]
    return matches[-1]["value"] if matches else ""


def xim_selection_owner(display_name: str) -> int:
    """Fcitx's XIM selection must exist before GPUI initializes its X11 client."""
    x11 = ctypes.CDLL("libX11.so.6")
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    x11.XInternAtom.restype = ctypes.c_ulong
    x11.XGetSelectionOwner.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    x11.XGetSelectionOwner.restype = ctypes.c_ulong
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    xdisplay = x11.XOpenDisplay(display_name.encode())
    if not xdisplay:
        return 0
    try:
        atom = x11.XInternAtom(xdisplay, b"@server=fcitx", 1)
        return x11.XGetSelectionOwner(xdisplay, atom) if atom else 0
    finally:
        x11.XCloseDisplay(xdisplay)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--packages", type=Path, default=Path(os.environ.get("BROXSER_IME_PACKAGES", "/tmp/broxser-ime-runtime/packages")))
    parser.add_argument("--runtime", type=Path, default=Path("/tmp/broxser-ime-runtime/root"))
    parser.add_argument("--database", type=Path, default=Path("/var/lib/pacman/sync/extra.db"))
    parser.add_argument("--output", type=Path, default=Path(f"/tmp/broxser-ime-{time.strftime('%Y%m%d-%H%M%S')}"))
    parser.add_argument("--display", help="reuse an existing private X display, e.g. :97")
    parser.add_argument("--desktop", type=Path, default=os.environ.get("BROXSER_DESKTOP_BIN"),
                        help="built Broxser desktop executable (or BROXSER_DESKTOP_BIN)")
    parser.add_argument("--browser", type=Path, default=os.environ.get("BROXSER_HELIUM_BIN"),
                        help="Helium executable (or BROXSER_HELIUM_BIN)")
    parser.add_argument("--click", default="auto", help="first input position X,Y relative to the X window; default scans test canvas")
    parser.add_argument("--capture-private-root", action="store_true",
                        help="capture candidate popup from the whole display ONLY when it is a private Xvfb display")
    parser.add_argument("--scenario", choices=("basic", "extended"), default="basic",
                        help="extended also checks repeat, Escape cancellation, Latin commits and textarea isolation")
    parser.add_argument("--manual-seconds", type=int, default=0, help="pause for manual IME and device switching checks")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--inside-dbus", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.capture_private_root and args.display:
        raise RuntimeError("omit --display for full-display capture: the probe must own its Xvfb server")

    if not args.inside_dbus and not args.prepare_only:
        # Fcitx and fcitx5-remote must share this private bus. No host daemon is touched.
        return subprocess.call(["dbus-run-session", "--", sys.executable, __file__, *sys.argv[1:], "--inside-dbus"])

    checked_runtime(args.packages, args.runtime, args.database)
    if args.prepare_only:
        return 0
    desktop = args.desktop
    helium = args.browser
    if desktop is None or not desktop.is_file() or not os.access(desktop, os.X_OK):
        raise RuntimeError("pass --desktop (or BROXSER_DESKTOP_BIN) with an executable desktop binary")
    if helium is None or not helium.is_file() or not os.access(helium, os.X_OK):
        raise RuntimeError("pass --browser (or BROXSER_HELIUM_BIN) with an executable Helium binary")
    desktop = desktop.resolve()
    helium = helium.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    env = isolated_env(args.output, args.runtime)
    env["BROXSER_HELIUM_BIN"] = str(helium)
    processes: list[subprocess.Popen] = []
    logs = []
    server = None

    def start(cmd, name, *, run_env=env):
        log = (args.output / f"{name}.log").open("w")
        logs.append(log)
        proc = subprocess.Popen(cmd, cwd=REPO, env=run_env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        processes.append(proc)
        return proc

    try:
        if args.display:
            env["DISPLAY"] = args.display
        else:
            for number in range(91, 120):
                if not Path(f"/tmp/.X11-unix/X{number}").exists():
                    env["DISPLAY"] = f":{number}"
                    break
            else:
                raise RuntimeError("no unused X display from :91 to :119")
            start([str(args.runtime / "usr/bin/Xvfb"), env["DISPLAY"], "-screen", "0", "1400x900x24", "-nolisten", "tcp"], "xvfb")
        xdotool = str(args.runtime / "usr/bin/xdotool")
        wait_until(lambda: subprocess.run([xdotool, "getdisplaygeometry"], env=env, capture_output=True).returncode == 0,
                   10, "X display did not start; see xvfb.log")
        print(f"private DISPLAY={env['DISPLAY']}", flush=True)

        fcitx = start(["/usr/bin/fcitx5", "-D", "--ui=classic", "--disable", "cloudpinyin",
                       "--verbose", "key_trace=5"], "fcitx")
        def ready():
            if fcitx.poll() is not None:
                raise RuntimeError("Fcitx exited; see fcitx.log")
            return subprocess.run(["fcitx5-remote", "--check"], env=env, capture_output=True).returncode == 0
        wait_until(ready, 15, "Fcitx did not register on the private bus")
        owner = wait_until(lambda: xim_selection_owner(env["DISPLAY"]), 10,
                           "Fcitx did not claim @server=fcitx before GPUI startup")
        print(f"Fcitx XIM selection owner: 0x{owner:x}", flush=True)
        subprocess.run(["fcitx5-remote", "-s", "pinyin"], env=env, check=True)
        print("Fcitx XIM ready; Pinyin configured (active method appears after field focus)", flush=True)

        event_file = args.output / "events.jsonl"
        request_file = args.output / "requests.log"
        handler = functools.partial(Events, directory=str(REPO / "examples/fixture"), event_file=event_file, request_file=request_file)
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{server.server_address[1]}/ime.html"
        print(f"fixture: {url}", flush=True)
        app = start([str(desktop), "--workspace", "examples/workspace.json", "--url", url], "desktop")
        def window_id():
            if app.poll() is not None:
                raise RuntimeError("desktop exited; see desktop.log")
            result = subprocess.run([xdotool, "search", "--pid", str(app.pid), "--name", "^Broxser$"], env=env, capture_output=True, text=True)
            return result.stdout.splitlines()[0] if result.returncode == 0 and result.stdout.strip() else None
        window = wait_until(window_id, 25, "Broxser window did not open")
        subprocess.run([xdotool, "windowsize", window, "1360", "861"], env=env, check=True)
        wait_until(lambda: request_file.exists() and "/ime.html" in request_file.read_text(), 30,
                   "fixture was not requested by Helium")
        time.sleep(2)
        screenshot = args.output / "screen.png"
        if capture_owned_window(screenshot, app.pid, window, env):
            print(f"owned-window screenshot: {screenshot}", flush=True)
        else:
            print("owned-window screenshot unavailable; event log remains authoritative", flush=True)
        print(f"event log: {event_file}", flush=True)
        if args.manual_seconds:
            print(f"manual check for {args.manual_seconds}s. Use DISPLAY={env['DISPLAY']} {xdotool} to interact.", flush=True)
            print("Check commit, repeat, Escape cancel, textarea/contenteditable, scrolled caret, switch/hide and close.", flush=True)
            time.sleep(args.manual_seconds)
        else:
            def key(name: str):
                # Xvfb has no window manager. Explicit focus keeps physical XTest
                # key events on the GPUI window while Fcitx shows its popup.
                subprocess.run([xdotool, "windowfocus", "--sync", window], env=env, check=True)
                subprocess.run([xdotool, "key", "--clearmodifiers", name], env=env, check=True)
                time.sleep(0.18)

            def compose(word: str, action: str, target: str,
                        screenshot_name: str | None = None):
                before = len(events(event_file))
                for letter in word:
                    key(letter)
                expected_preedit = {"nihao": "ni hao", "n": "n"}[word]
                wait_until(lambda: any(e.get("pageId") == page_id
                                       and e.get("target") == target
                                       and e.get("type") == "compositionupdate"
                                       and e.get("data") == expected_preedit
                                       for e in events(event_file)[before:]),
                           5, f"full {word!r} preedit did not reach {target}")
                time.sleep(0.5)  # allow the next Helium frame and native popup to settle
                if screenshot_name and args.capture_private_root:
                    candidate = args.output / screenshot_name
                    if capture_private_display(candidate, env):
                        print(f"candidate screenshot: {candidate}", flush=True)
                key(action)

            subprocess.run([xdotool, "windowfocus", "--sync", window], env=env, check=True)
            if args.click == "auto":
                point = None
                for y in (200, 250, 300, 350, 400, 450, 500, 550, 600, 650, 700):
                    for x in (520, 620, 720, 820, 920, 1020):
                        before = len(events(event_file))
                        subprocess.run([xdotool, "mousemove", "--window", window, str(x), str(y), "click", "1"],
                                       env=env, check=True, capture_output=True)
                        time.sleep(0.15)
                        if any(e.get("target") == "one" and e.get("type") == "click"
                               for e in events(event_file)[before:]):
                            point = (x, y)
                            break
                    if point:
                        break
                if point is None:
                    raise RuntimeError("automatic canvas scan never reached the first input; inspect events.jsonl")
                print(f"first input at X-window coordinate {point}", flush=True)
            else:
                x, y = args.click.split(",", 1)
                subprocess.run([xdotool, "mousemove", "--window", window, x, y, "click", "1"], env=env, check=True)
                wait_until(lambda: any(e.get("target") == "one" and e.get("type") == "click"
                                       for e in events(event_file)), 3,
                           "click missed the first input; inspect event log and pass --click X,Y")
            selected = next(e for e in reversed(events(event_file))
                            if e.get("target") == "one" and e.get("type") == "click")
            page_id = selected["pageId"]
            print(f"selected fixture page={page_id}, viewport={selected['viewport']}", flush=True)
            time.sleep(0.25)
            subprocess.run([xdotool, "windowfocus", "--sync", window], env=env, check=True)
            subprocess.run(["fcitx5-remote", "-s", "pinyin"], env=env, check=True)
            subprocess.run(["fcitx5-remote", "-o"], env=env, check=True)
            def active_pinyin():
                state = subprocess.check_output(["fcitx5-remote"], env=env, text=True).strip()
                current = subprocess.check_output(["fcitx5-remote", "-n"], env=env, text=True).strip()
                if state == "2" and current == "pinyin":
                    return state, current
                # Creating/focusing the XIM context is asynchronous. An early
                # activation can precede CreateIC and affect no context at all.
                subprocess.run(["fcitx5-remote", "-s", "pinyin"], env=env, check=True)
                subprocess.run(["fcitx5-remote", "-o"], env=env, check=True)
                return None
            state, current = wait_until(active_pinyin, 4, "Fcitx Pinyin did not activate on the focused Broxser window")
            print(f"Fcitx state={state}, input method after focus={current!r}", flush=True)
            compose("nihao", "space", "one", "candidate-input.png")
            try:
                wait_until(lambda: latest_value(event_file, page_id, "one") == "你好", 4,
                           f"first Pinyin commit missing; last value={latest_value(event_file, page_id, 'one')!r}")
            except RuntimeError:
                found = events(event_file)
                compositions = [e for e in found if e.get("pageId") == page_id
                                and e.get("type") == "compositionstart"]
                print(f"events={len(found)} compositionstart={len(compositions)} first value={latest_value(event_file, page_id, 'one')!r}", flush=True)
                raise
            has_dom_composition = any(e.get("pageId") == page_id and e.get("type") == "compositionstart"
                                      for e in events(event_file))
            print(f"first 你好 commit passed; DOM compositionstart={has_dom_composition}", flush=True)
            if args.scenario == "extended":
                compose("nihao", "space", "one", "candidate-repeat.png")
                wait_until(lambda: latest_value(event_file, page_id, "one") == "你好你好", 4,
                           "repeated identical Pinyin commit was lost or duplicated")
                compose("nihao", "Escape", "one", "candidate-cancel.png")
                time.sleep(0.5)
                if latest_value(event_file, page_id, "one") != "你好你好":
                    raise RuntimeError("Escape cancellation changed the input value")
                for expected in ("你好你好n", "你好你好nn"):
                    compose("n", "Return", "one")
                    wait_until(lambda: latest_value(event_file, page_id, "one") == expected, 4,
                               f"single-ASCII Pinyin commit missing; expected {expected!r}")
                print("repeat, cancellation and repeated single-ASCII commits passed", flush=True)
                textarea_point = None
                for y in (300, 350, 400, 450, 500, 550, 600, 650, 700):
                    for x in (520, 620, 720, 820, 920, 1020):
                        before = len(events(event_file))
                        subprocess.run([xdotool, "mousemove", "--window", window, str(x), str(y), "click", "1"],
                                       env=env, check=True, capture_output=True)
                        time.sleep(0.15)
                        if any(e.get("pageId") == page_id and e.get("target") == "two"
                               and e.get("type") == "click" for e in events(event_file)[before:]):
                            textarea_point = (x, y)
                            break
                    if textarea_point:
                        break
                if textarea_point is None:
                    raise RuntimeError("could not find textarea on the selected fixture page")
                print(f"same-page textarea at X-window coordinate {textarea_point}", flush=True)
                compose("nihao", "space", "two", "candidate-textarea.png")
                wait_until(lambda: latest_value(event_file, page_id, "two") == "你好", 4,
                           "Pinyin commit into textarea missing")
                if latest_value(event_file, page_id, "one") != "你好你好nn":
                    raise RuntimeError("textarea composition leaked into the first input")
                print("textarea commit and input isolation passed", flush=True)
            if not has_dom_composition:
                raise RuntimeError("native Pinyin committed, but fixture saw no DOM compositionstart; inspect event log")
        subprocess.run([xdotool, "key", "ctrl+q"], env=env, check=False)
        wait_until(lambda: app.poll() is not None, 15, "Broxser did not close after Ctrl+Q")
        if app.returncode != 0:
            raise RuntimeError(f"Broxser exited with code {app.returncode}")
        profiles = list((args.output / "tmp").glob("broxser-cdp-*"))
        if profiles:
            raise RuntimeError("Broxser left a browser profile after closing")
        (args.output / "summary.json").write_text(json.dumps({
            "ime": "Fcitx5 Pinyin over XIM (UseOnTheSpot)",
            "scenario": "manual" if args.manual_seconds else args.scenario,
            "display": env["DISPLAY"], "exit_code": app.returncode,
            "browser_profiles_remaining": len(profiles),
            "fixture_events": len(events(event_file)),
        }, indent=2) + "\n")
        print("IME probe passed", flush=True)
        return 0
    finally:
        if server:
            server.shutdown()
            server.server_close()
        for proc in reversed(processes):
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
        for proc in reversed(processes):
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
        for log in logs:
            log.close()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"IME probe failed: {error}", file=sys.stderr)
        sys.exit(1)
