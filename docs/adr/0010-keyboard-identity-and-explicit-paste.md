# ADR 0010 Keyboard identity, browser keys and explicit paste

Status: accepted for P1.3 (keyboard, shortcuts and clipboard), 2026-09-25.
Amends ADR 0005 (input) and ADR 0006 (held keys of hidden devices). IME
composition is not decided here.

## Context

GPUI 0.2.2 reports a key as `key`, `key_char` and modifiers, without a physical
keycode. On Linux the key name follows the Shift and compose state at the time of
the event, a composed character is named after its keysym (`eacute`), X11
repeats arrive as key-downs with `is_held` false, and text from an input method
reaches only a registered input handler. Broxser paired a release with its press
by name, typed `key` as text when `key_char` was missing, forwarded only a fixed
set of named keys, and forwarded every other key, Ctrl+V and middle clicks to the
page.

Reproduced on `main` at `4c2d5c1` in a real X11 window (Xvfb, xdotool, a fixture
page reporting key, input and paste events). This Xvfb ignores keymap changes
from clients, so a second server ran with a private copy of the XKB data whose
default layout is German.

| Action | Result on `4c2d5c1` |
| --- | --- |
| German Shift+7 (`/`), Shift released first | GPUI reports `/` on press and `7` on release. The keyup reached the page 368 ms late, only when another device was selected; afterwards both further `/` presses were dropped, because the stale name stayed suppressed |
| German dead keys `´` then `e`, `^` then `a` | `é` and `â` were dropped (GPUI names them `eacute`, `acircumflex`); one dead key typed `'`, GPUI's ASCII guess for its position |
| F2, Insert, F12 | Delivered by GPUI, never forwarded |
| Ctrl+V with text in the system clipboard | The page received an empty `paste` event: Chromium pasted its own clipboard |
| Ctrl+C in the Guest phone, Ctrl+V in the Admin desktop | The Admin page received `secret-guest` |
| Text only selected in the Guest phone, middle click in the Admin desktop | The Admin page received the selected text |
| Ctrl+W in the Guest phone | The Guest tablet's tab closed ("The page target was detached."): Helium closes the active tab of the window both devices share |
| Ctrl+Shift+M in the Guest phone | The whole runtime stopped: the browser dropped the CDP connection |
| Ctrl+U in the Guest phone | The page was requested again, for a view-source tab Broxser never showed |
| Ctrl+R | One reload and no key event in the page: application shortcuts run first |

The headless browser keeps one clipboard and one selection buffer for all
BrowserContexts, so copying or merely selecting in one session was readable by
pasting in another. `navigator.clipboard.readText()` is denied in these pages
("Read permission denied", with a user gesture); `writeText()` succeeds.

Each key below was then sent through the engine to a fresh Helium 0.18.1.1
(Chromium 154.0.8037.57, `--headless=new`), focused in a text area and in the page
body:

| Effect | Keys |
| --- | --- |
| Closes the device's tab | Ctrl+W, Ctrl+F4 |
| Closes its window, with every device of the session in it | Ctrl+Shift+W, Alt+F4 (in the tablet, the phone went too) |
| Stops the browser | Ctrl+Shift+M |
| Opens a tab, browser page or UI Broxser never shows | Ctrl+T, Ctrl+N, Ctrl+Shift+N (new tab page), Ctrl+U (view source, page requested again), Ctrl+J (downloads), Ctrl+Shift+O (bookmarks), Ctrl+Shift+Delete (settings), Ctrl+Shift+A (tab search) |
| Opens DevTools, which requests `/.well-known/appspecific/com.chrome.devtools.json` from the page's origin | F12, Ctrl+Shift+I, Ctrl+Shift+J |
| Reloads | F5, Shift+F5, Ctrl+F5, Ctrl+R, Ctrl+Shift+R |
| Navigates history or home | Alt+Left, Alt+Right, Alt+Home (new tab page) |
| Nothing visible | F1, F3, F6, F7, F10, F11, Escape, Shift+Escape, Ctrl+H, Ctrl+D, Ctrl+P, Ctrl+S, Ctrl+O, Ctrl+F, Ctrl+G, Ctrl+L, Ctrl+K, Ctrl+E, Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+PageUp, Ctrl+PageDown, Ctrl+Shift+PageDown, Ctrl+1, Alt+1, Ctrl+`=`/`-`/`0`, Ctrl+Shift+B, Ctrl+Shift+C, Ctrl+Shift+P, Alt+D, Alt+E, Alt+F, Ctrl+Shift+Q twice |

In the text area the page received the key-down before Helium acted, except for
Ctrl+W, Ctrl+F4, Ctrl+Shift+W and Alt+F4, which never reached the page.

## Options

| Option | Assessment |
| --- | --- |
| Pair releases by name only | Leaves keys stuck and suppressed whenever the name changes |
| Guess the physical key from a US table | Wrong for most layouts; GPUI already does this for non-ASCII keysyms only |
| Track every press in the window and pair an unmatched release with the latest press whose name can change | X11 pairs every release with its press; letters and named keys keep their names, so only digit, symbol and composed keys are candidates |
| Let Helium act on unhandled keys, as Chrome does | Closes devices across a session, crashes the runtime, opens tabs Broxser does not track and reloads outside its commands; CDP has no switch to skip browser handling |
| Suppress Helium's handling from a page script that marks the event handled | Changes what the page itself observes; reserved keys act before any script |
| Do not forward keys that Helium turns into browser commands | Pages lose those keys; the list depends on the Helium version |
| Forward Ctrl+V and let the page paste | Pastes the browser's shared clipboard, across sessions |
| Paste the system clipboard as inserted text into the selected visible device | Explicit user action, visible target, no shared browser clipboard; no `paste` event or rich content |
| One browser per session | Separate clipboards and windows, but a larger runtime and lifecycle change than this gap needs |

## Decision

- **Presses.** Every key-down in the window is recorded before shortcuts and
  elements handle it (a GPUI keystroke interceptor), by name and in press order.
  Only the key with the latest key-down repeats, as X11 repeats only the last
  key pressed: a key-down is a repeat only if its key had the latest key-down.
  A key-up ends the press under its name or, failing that, the most recently
  pressed key whose name can change (digits, symbols, dead keys and composed
  characters, not letters or named keys). Nothing is sent later or replayed.
- **Held keys.** A key held in a page that is hidden, deselected, loses focus or
  restarts is released in that page at once and stays pressed; its repeats go
  to no page, the new one included, until its release.
- **Dead keys.** A key without a character and without Ctrl, Alt or Meta is a dead
  key or compose step. It is forwarded as DOM key `Dead` without text, never as the
  guessed ASCII character. A composed character is typed as that character; its
  `code` is unknown and left empty.
- **More keys.** F1–F12 and Insert are forwarded with their DOM key, code and key
  code, except the browser keys below.
- **Browser keys.** The engine never forwards the keys measured above to close
  tabs or windows (Ctrl+W, Ctrl+F4, Ctrl+Shift+W, Alt+F4), stop the browser
  (Ctrl+Shift+M), open tabs, pages or DevTools (Ctrl+T, Ctrl+N, Ctrl+Shift+N,
  Ctrl+U, Ctrl+J, Ctrl+Shift+O, Ctrl+Shift+Delete, Ctrl+Shift+A, F12, Ctrl+Shift+I,
  Ctrl+Shift+J), reload (F5 with any modifier, Ctrl+R, Ctrl+Shift+R) or navigate
  (Alt+Left, Alt+Right, Alt+Home), nor Ctrl+Shift+Q (quit) and Ctrl+Shift+T (reopen a
  closed tab). F5 reloads the selected device in Broxser, like Ctrl+R (ADR 0008).
- **Paste.** Ctrl+V, Shift+Insert and Ctrl+Shift+V in a device read the system
  clipboard's text and insert it into the selected visible device's focused element
  with `Input.insertText`, at most 65,536 characters, without control characters
  other than tab and line breaks. Holding the keys pastes once. Nothing is pasted
  without that key press, and not into hidden or unresponsive devices (ADR 0008).
- **Shared browser clipboard.** The engine never forwards those paste keys or
  middle-button presses to a page, nor the middle button in the button state of
  other pointer events, so Chromium's clipboard and selection buffer, shared by
  all sessions, cannot be pasted from.
- Application shortcuts (Ctrl+Q, Ctrl+R, F5, Ctrl+L) keep running before page input.

## Consequences and limits

- Pages never receive the browser keys, although Chrome lets pages handle most of
  them first: an editor's Ctrl+U, or a page's own F5 or Alt+Left, does nothing.
- The list holds for Helium 0.18.1.1. A Helium update must repeat the measurement
  (the live test covers the listed keys); keys that showed no effect, such as
  Ctrl+Tab, Ctrl+1, Ctrl+H, F1 or Ctrl+Shift+C, are forwarded and may need adding.
- Pasted text arrives as inserted text: the page sees `beforeinput` and `input`,
  not a `paste` event, and no HTML, files or images.
- Copy and cut still run in the page but reach only the browser's clipboard;
  copying to the system clipboard is not supported yet.
- Reading the system clipboard is GPUI's synchronous platform call on the UI
  thread. On X11 a clipboard owner that does not answer blocks the window for up
  to 4 seconds, as in the URL field.
- Middle clicks do nothing in pages: no selection paste, no link in a new tab
  (popups are not shown anyway) and no autoscroll.
- When several keys with changeable names are held and one changes its name, a
  release can end the wrong one of them early; no key stays stuck. A key whose name
  changes while it repeats (Shift released while a shifted symbol repeats) repeats
  under the new name as a new press, as before this change.
- DOM `code` and key codes stay empty for symbols and composed characters, and
  punctuation codes follow no layout.
- Input methods (ibus, fcitx over XIM) still type nothing: their text reaches only
  an input handler, which the device canvas does not register yet. This needs its
  own decision on composition and caret placement and an IME to verify with.

## Validation

Engine tests cover the key mapping, paste keys and browser keys; Helium tests
cover inserted text, dropped paste keys and middle buttons across sessions, hidden
devices, and every listed browser key: no tab or window closes, no tab opens, no
page reloads or navigates, and the runtime keeps running. Desktop tests cover
press tracking, repeats and release pairing. The X11 reproducers above were rerun
with the change; see `docs/validation.md`.
