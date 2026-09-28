# ADR 0022 Workspace management without credentials: a draft in a panel, device presets and an application state file

Status: accepted for P2.2 (first part), 2026-09-27. Builds on ADR 0002
(portable workspace) and ADR 0009 (one restart at a time). Persistent sessions
are ADR 0021, proposed and separate.

## Context

A workspace is a JSON file handed to the desktop with `--workspace`; without
it the demo workspace opens. Nothing in the window edits it: adding a device
means editing the file by hand and starting again, and the next start forgets
which file was open and how big the window was. `GOALS.md` (P2.2) asks for a
UI to manage projects, devices and presets and for configuration restore,
with the portable JSON kept free of cookies, tokens and passwords.

Constraints found while auditing the desktop:

- The live runtime is started from the workspace and addresses devices by
  index; changing the device list under a running runtime would misdirect
  frames and input. A restart already rebuilds everything from the
  workspace (ADR 0009).
- GPUI's file dialogs on Linux go through the desktop portal (`ashpd`), which
  is missing on some machines and in the test container; a first version
  cannot depend on them.
- A page's current address can carry a code or a token (ADR 0013), so it is
  not configuration to remember.

## Options

| Option | Assessment |
| --- | --- |
| Edit the running workspace in place | Frames and input are addressed by device index; edits would race the runtime |
| Edit a draft, apply it by restarting the runtime | The restart path exists and is serialized; no edit reaches a running page |
| Free-form device fields | Needs text inputs, validation feedback and a design of its own; exact sizes stay editable in the file |
| Device presets by viewport class | A few buttons cover the common cases; generic names, no product copy |
| Remember the last workspace and window in the workspace file | Mixes machine state into a shareable file |
| A separate application state file | Local, small, and holds nothing a page could plant |

## Decision

- The desktop keeps a **draft** of the workspace. The panel (toolbar button
  "Workspace", `Ctrl+Shift+W`) edits the draft: add a device from a preset
  into the selected device's session, remove a device (never the last one),
  discard. **Apply** restarts the runtime with the draft, through the
  existing restart; **Save** validates the draft and writes it atomically to
  the file it came from, including the URL bar's address, and never touches
  the runtime. An invalid URL reports an error and leaves the file unchanged.
  Save captures the draft and address at the click, runs file I/O off the GUI
  thread, and accepts no second Save until the first finishes. The demo
  workspace has no file, and the panel says so.
- `broxser-core` offers eight **presets**, generic viewport classes ("Small
  phone" 360 × 640 at 2×, up to "Large desktop" 1920 × 1080 at 1×), and
  `add_device_from_preset` / `remove_device` that keep ids unique, number
  duplicate names using the first unused candidate, validate the result and
  leave the workspace unchanged on failure (the pixel budget of ADR 0002 included).
- An **application state** file (`$XDG_STATE_HOME/broxser/state.json`, else
  `~/.local/state/broxser/state.json`; `BROXSER_STATE_FILE` overrides it for
  tests) remembers the recent workspace files, most recent first and at most
  ten, and the window size. Without `--workspace` the desktop opens the most
  recent file that still exists, else the demo. The file is validated like a
  workspace (schema version, absolute paths, bounded sizes) and replaced
  atomically; an unreadable one is reported on stderr and ignored. It holds no
  page address, cookie, token or profile path.
- Close reads and saves the window state off the GUI thread and begins browser
  cleanup independently. The window stays responsive until cleanup and any
  pending workspace/state saves finish; repeated close requests start no
  additional writes. Close during Apply still prevents a replacement browser.
  At most two dedicated save threads run, so slow disk I/O cannot occupy GPUI's
  finite worker pool used for frame decoding and browser cleanup.
- A decoded frame checks its runtime generation before looking up its device;
  a missing index is discarded too. Apply can remove or reorder devices while
  a previous runtime's decode is still running.
- Static mode keeps its command line; the panel is part of the live view.

## Consequences

- A device added in the panel exists after Apply, as a fresh runtime with the
  draft's devices; hidden flags and the selection start over, as after any
  restart. Nothing changes for the running pages until then.
- Save writes what was in the panel and the URL bar, so a workspace file can
  gain a device without leaving the app; exact sizes, names and sessions are
  still edited in the file.
- The state file is per user and per machine; sharing a workspace file shares
  no state. Deleting the state file only forgets the recent list and size.
- Presets carry generic names by design; a team's exact devices belong in
  the workspace file.

## Validation

Unit: `presets_add_named_unique_devices_and_roll_back_over_budget` and
`application_state_remembers_workspaces_and_the_window_without_secrets` in
`broxser-core`. Real window: the desktop smoke run "workspace panel edits a
draft and saves it" opens the panel with `Ctrl+Shift+W`, adds the first
preset and applies it (four devices load the page), removes the first device
and applies again (three), saves the copy of the example workspace and checks
it, then closes and checks the state file. Results are in `docs/validation.md`
(P2.2a).
