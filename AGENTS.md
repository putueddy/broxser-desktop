# Broxser engineering rules

Linux first. Use GPUI for native application UI and the Helium adapter for web
rendering. Read README.md, docs/system-design.md and relevant ADRs before edits.

- Keep broxser-core independent of GPUI, CDP, networking and browser processes.
- Confine CDP payloads and browser lifecycle to broxser-engine. Keep blocking work
  off the GUI thread. No browser subprocess per frame.
- Ship truthful states. Static captures must not be described as interactive
  webviews; a core sync router is not a wired end-to-end sync feature.
- Use only application-owned private browser profiles. Keep the browser sandbox
  enabled and the debug endpoint local. Never connect to a personal profile.
- Treat workspace files and page events as untrusted. Validate before side effects.
- Never replay click, form, authentication or payment events automatically after
  reconnect. Navigation itself can have effects; retries must be explicit.
- Pin reproducible dependencies, then update them deliberately. Record engine
  version/checksum/license changes with smoke evidence. Do not hold an old browser
  release indefinitely in the name of compatibility.
- Preserve existing user changes. Avoid speculative abstractions and dependencies.
- Run `bash scripts/check.sh`; engine changes also require the live Helium test.
  GUI changes require a real Linux window check, not compilation alone.
- Do not add Sizzy binaries, extracted code, artwork or copied product copy.
- Keep generated browser profiles, captures and credentials outside Git.

Delegate bounded independent work when useful: explorer/researcher for evidence,
worker for implementation, reviewer for consequential uncertainty. The root owns
integration and final verification. Editing agents must have explicit file ownership
and preserve concurrent work. Do not delegate further without the root's request.
