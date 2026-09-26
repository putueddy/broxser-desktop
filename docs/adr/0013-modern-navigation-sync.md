# ADR 0013 Link sync across redirects, same-document navigation and subframes

Status: accepted for P1.5, 2026-09-26. Amends ADR 0006.

## Context

ADR 0006 synchronizes a link only when the loader of a trusted link activation
commits at exactly the link's URL, and left redirects, hash and SPA navigation and
subframes for their own decision. A CDP probe against Helium 0.18.1.1 (Chrome 154)
and a survey of `main` at `e2bf9cb` with the live test fixture found:

- **Redirects.** A link whose server answers 302 or 307, once or in a chain, to the
  same or another origin, commits on the link's own loader with the final URL in
  `Page.frameNavigated`. Broxser compared that URL with the link and never
  synchronized: the phone landed while the tablet stayed. The link's URL is the
  only one the user chose; the final URL can carry a per-session code or token.
- **Fragments.** `frame.url` of `Page.frameNavigated` omits the fragment and reports
  it in `frame.urlFragment`. A link to `/landed#part`, or a redirect to it, never
  matched the link URL, and the device status showed `/landed` without the fragment.
- **Same-document navigation.** A hash link, a router that calls `history.pushState`
  or `history.replaceState` from the link's click handler, and a Navigation API
  `intercept` produce only `Page.navigatedWithinDocument` (types `fragment`,
  `historyApi` and `other`). There is no `Page.frameRequestedNavigation`, no
  loader and no `beforeunload`, so the isolated observer's confirmation phase never
  runs. The observer's activation report is the only evidence, and the event
  cleared it. Nothing synchronized, while the phone showed the new URL.
- **Subframes.** The observer script runs in every frame, and a frame's report
  arrives from the frame's own isolated context. Broxser accepted reports only from
  the registered main-frame context and ignored frame events, so a link inside an
  iframe, a hash or History API change inside it, and a main-document link with
  `target` set to the frame never synchronized. This was already correct, but
  untested.
- **Cancelled and superseded navigations.** A 204 answer, a download, a dropped
  connection and `window.stop()` end without `Page.frameNavigated`; a second link
  replaces the first candidate; Go and a script's `location.href` replace the link's
  loader; an unreachable host commits `chrome-error://chromewebdata/` with the link
  in `unreachableUrl`. None of these synchronized, the last one only because the
  URLs differed.
- **Script-generated events.** `a.click()` from a script is not trusted, a button
  that pushes a URL or sets `location.hash` has no activation, and a router that
  pushes a URL other than the link's does not reach the link's URL. None of them
  synchronized.

The unguarded first probe run also reproduced ADR 0004: Helium's bundled blocker
reloaded a redirected page 2.5 s after it committed and held another redirect for
2.3 s. With the seeded profile preference, none of this happened.

## Decision

- **The link URL is what synchronizes.** When the link's own loader commits, not at
  an error page, peers navigate to the link's URL and follow their own redirects.
  The committed URL, including its fragment, is shown for the device but never
  sent to a peer. Peers can land elsewhere, for example on a per-session redirect,
  and each device reports where it is.
- **Same-document navigation follows a live activation.** A main-frame
  `Page.navigatedWithinDocument` whose URL equals the latest trusted link
  activation's URL synchronizes that URL, once, if the activation belongs to the
  current document and happened within the follow window (10 s; routers push
  after their data arrives). A change to another URL, such as a router saving
  state on the current entry with `replaceState` first, keeps the activation. A new
  document, hiding the device, an activation before navigation sync was switched
  on or off, a report from another frame, a newer activation and the window's end
  all retire it. Peers receive `Page.navigate` with the URL: for a fragment of
  their current document Chromium performs a same-document navigation without a
  request; for a route they load the document from the server, which an
  application with client-side routing serves.
- **Subframes never synchronize.** Reports and navigation events of frames other
  than the main frame are ignored, now covered by tests.
- Page-started cross-document navigations (`scriptInitiated`, meta refresh,
  forms), `history.back()` and `forward()`, modified clicks, downloads, new tabs
  and navigations that end without a document remain outside the contract.

## Consequences

- A peer requests the link with its own cookies, as it did before; a redirect
  target with a code or token stays on the device that received it.
- Fragments now appear in device status URLs.
- A route reached through a router is loaded by peers as a full document. An
  application whose server does not serve its client-side routes shows its 404
  on the peers, truthfully.
- The follow window bounds the time between a click and a router's push. A page
  that pushes the link's URL within it for another reason synchronizes; a router
  slower than 10 s does not.

## Validation

Fake CDP: `redirected_link_commit_synchronizes_the_link_and_keeps_fragments`
(redirect commit, fragment, error page, another loader) and
`same_document_link_navigation_follows_only_a_live_activation` (router
`replaceState` then `pushState`, one activation once, subframe events and
reports, replaced, stale, expired and hidden activations, sync toggling, hash).
Helium: `live_link_sync_follows_redirects_and_fragments_with_the_link_url`,
`live_same_document_link_navigations_sync_within_the_session`,
`live_script_and_stale_same_document_changes_never_sync`,
`live_subframe_navigations_never_sync` and
`live_cancelled_and_superseded_link_navigations_sync_at_most_the_latest`. Results
and the probe transcript summary are in `docs/validation.md` (P1.5).
