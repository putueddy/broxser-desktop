# ADR 0003 Independent engine update lifecycle

Status: proposed operating policy, 2026-09-24.

A browser product remains useful for ten years only if someone maintains its engine.
Exact versions and checksums reproduce a release; they are not a reason to freeze
security dependencies. Keep Helium outside the application source tree and select it
through a small manifest or explicit executable path.

Assign a primary and backup owner. Review advisories weekly and urgent ones within
one business day; target critical qualification within 72 hours of a usable upstream
release. Test startup, lifecycle, physical dimensions, storage isolation, permissions,
cleanup and representative company applications before promotion. Native shell tests
are separate so a browser update need not force a GPUI upgrade.

Record each qualified engine's upstream source, digest, protocol/capability results,
license inventory and release artifact. Pilot first, then promote. Keep configuration
backups and rehearse rollback; do not roll back blindly to a vulnerable browser or
downgrade a newer profile. Re-evaluate maintenance costs and upstream health annually.
