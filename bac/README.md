# BAC fork maintenance

## What this directory is

`bac/` contains the fork's upstream inventory, dependency security floors, maintenance tools and [release-sync procedure](SYNC.md). `UPSTREAM_BASE.json` identifies the upstream release against which differences are reviewed.

## Compatibility rules

**Wire:** preserve upstream `bat-remote/v2` channels, events, payloads, errors, limits and `serverVersion`. Extensions use only `bac:*` plus one additive `bac` capability; send extension events only to clients that opt in.

**Data:** keep upstream file formats unchanged. Store extension data under `<data-dir>/bac/` so a host can switch back to the same upstream version.

**Merges:** put features in our own modules, crates and i18n namespace. Keep changes in upstream files small and explained. Never reformat, rename, reorder, clean up or fix unrelated upstream code, hand-edit locks, or reshape existing IPC contracts.

## Checks

Run from the repository root with Node 24, the pinned pnpm, Rust and installed dependencies:

```sh
node bac/tools/check-upstream-inventory.mjs
node bac/tools/check-security-floor.mjs
python3 scripts/verify-vendored-glib.py
node --test bac/tools/tests/*.test.mjs
```

The inventory rejects unlisted differences, stale entries, changed diff digests, hook-marker inconsistencies and own-path collisions. It checks staged and unstaged content. `--update` refreshes counts and digests only for existing entries; review every resulting change.

The four basic rejection cases are an edit to an uninventoried upstream file, an inventoried file restored to upstream, an unacknowledged edit to an inventoried file, and a resolved dependency below its enforced floor. Tests cover each case; none can be accepted by silently adding an inventory reason.

Lock digests are not pinned. The inventory validates manifests with offline, frozen pnpm checks on scratch copies and `cargo metadata --locked --offline`; missing caches must be populated through the normal locked install/build. The floor reader checks every resolved version, rejects unsupported package-record formats, and enforces exact local backport provenance. The separate GLib verifier checks the patched source bytes. Open floor entries are reported explicitly.

## Add a hook

Put the implementation and tests in our own files first. Leave only the necessary call or registration in the upstream lifecycle, with a comment `bac-hook: <id>`. Use a unique ID matching `[a-z0-9][a-z0-9-]*`, declare it in that file's optional `hooks` list, and add its feature, kind, reason, plan and planReason to `upstream-inventory.json`.

Every marker must be declared, every declared ID must appear once, and IDs cannot be reused across files. Existing unmarked entries have no `hooks` list; the summary distinguishes marked and unmarked hook entries. Run `--update` and inspect the patch before committing. Larger existing changes remain `legacy-patch`; an inventory entry is not a compatibility certification.

## Add an own file

Add a file path and feature to the inventory's `own` list, or use an existing directory entry that covers it. Directory entries end in `/`, cover all files below them and collide if upstream adds any path beneath them. `bac/` is explicitly owned this way.

Use a directory entry only when the whole directory is ours. Do not nest ownership entries or list a file beneath an owned directory: each path has one owner. Empty directory entries are invalid. If upstream adopts an owned path, reconcile the implementations and ownership before accepting the new base.
