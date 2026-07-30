# Kira

**An app store for UNA Watch.** Browse apps, see what's on your watch, and
install or update over the USB cable — from a static web page, with no backend.
Every published version of every app is downloadable, with release notes, and
each version is marked according to whether the app's code actually changed.

> Unofficial. Not affiliated with, endorsed or sponsored by UNA Watch Ltd.
> See [THIRD-PARTY.md](THIRD-PARTY.md).

## How it works

There is no server. A GitHub Actions run fetches every published `apps-v*`
release from the [UNA SDK](https://github.com/UNAWatch/una-sdk), reads the
metadata out of each `.uapp` binary, and publishes a catalogue plus the binaries
to GitHub Pages. The page then talks to the watch directly as a USB mass-storage
volume.

Apps are never rebuilt here — the SDK's own CI already builds every app in its
ARM toolchain container, so Kira consumes those artifacts.

### One implementation, two runtimes

The interesting logic is Rust. `kira-core` reads the `.uapp` container, models the
catalogue, diffs it against a watch and generates installers; it does no I/O, so
the catalogue build links it natively and the browser loads the very same code
compiled to WebAssembly. There is no second implementation to drift.

```
crates/kira-core   the .uapp format, catalogue model, planner, script generation
crates/kira-cli    the `kira` binary: build, serve, icons
crates/kira-wasm   the browser surface, via wasm-bindgen
site/              the published page (app.js is DOM and filesystem glue only)
assets/            source artwork for the icons
```

JavaScript keeps only what a browser must do itself: the File System Access API,
IndexedDB and the DOM. Everything else crosses one narrow boundary — a `Store`
that owns the catalogue and the user's version selections.

### Everything comes from the binary

A `.uapp` is self-describing: a 48-byte header carries the AppID, version, the
LibC ABI version, type and autostart flags, the display name, and two embedded
icons. `kira-core` gives those real types — `AppId` renders as 16 hex digits,
`Version` packs into a `u32` that orders correctly, and the type is an enum
rather than two bits of a flag word.

The catalogue is keyed on **AppID**, never on folder or display name. Folder
names are arbitrary, and display names can contain a path separator — the
`GlanceARHR` app is really named `AVG / R HR`.

### Versions, and a changelog derived from bytes

App versions are **not per-app semver**. `una-version.sh` stamps every app in a
release with the `apps-v*` tag, so all thirteen apps in apps-v1.3.0 report
`1.3.0` whether or not their code changed — comparing the published binaries,
six of those thirteen are byte-identical to their apps-v1.2.0 builds.

Kira therefore hashes each version's payload (icons, service and GUI images, with
the version stamp and CRC footer excluded) and compares it with the next older
version. Each card says *"code changed in 1.3.0 (+17288 B)"* or *"code unchanged
since 1.2.0"*, and an update that is only a re-stamp is labelled *"version stamp
only, identical code"* rather than presented as new work. That changelog comes
from the binaries, not from prose.

Upstream's own release notes are shown too, per `apps-v*` tag, taken verbatim
from the GitHub release bodies. They are rendered as **text, never as HTML** —
it is third-party Markdown, and it is not going anywhere near `innerHTML`.

Any published version can be selected per app; the newest is the default.
Selecting an older one re-targets both the download and the installer, so a watch
already on the newest build correctly reports `newer-on-watch` rather than being
silently downgraded.

### Two capability tiers

|                          | Chrome / Edge / Opera | Firefox / Safari |
| ------------------------ | --------------------- | ---------------- |
| Browse the catalogue     | yes                   | yes              |
| Read what's on the watch | yes                   | yes              |
| Install and update       | yes, in-page          | generated script |

Writing to a removable drive needs the File System Access API, which only
Chromium desktop implements. Reading uses `<input webkitdirectory>`, which is
supported nearly everywhere — so Firefox and Safari still get the full inventory
and version diff, then a ready-to-run PowerShell or `sh` script that performs
exactly the writes the page planned.

### Installing safely

The install path follows the ordering proven by `Update-Watch-Apps.ps1` in the
SDK:

1. Download and check the binary **before** it touches the watch — expected
   size, SHA-256 against the catalogue, and the `.uapp` CRC-32 footer. A file
   failing CRC is dropped *silently* by the watch kernel, so the app would simply
   never appear.
2. Write the new `.uapp` into `Apps/<Folder>/`.
3. Read it back and check the length.
4. Only then delete any stale `.uapp` in that folder. The watch loads whichever
   it finds first, so leaving two can keep booting the old build.

App folders are never removed, so each app's `settings.json` and `Activity/` data
survive an update. Apps on the watch that aren't in the catalogue are reported and
left alone.

**Verification is a two-step affair.** Hashing straight after writing reads the
OS write cache and can report a false OK. Eject the watch, reconnect it, then
press *Verify flash* — Kira keeps the directory handle in IndexedDB so it can
re-check without you re-picking the drive. Then reboot the watch: the launcher
list is rebuilt only at boot.

## Development

Requires only a Rust toolchain; `rust-toolchain.toml` pins the compiler and pulls
in `wasm32-unknown-unknown`, clippy and rustfmt. `make wasm` additionally needs
`wasm-bindgen-cli` at the same version as the `wasm-bindgen` crate:

```sh
cargo install wasm-bindgen-cli --locked
```

Then:

```sh
make check          # fmt, clippy (including the wasm target) and tests
make wasm           # build the browser module into site/lib/
make serve          # build wasm, then serve site/ on :8099
make icons          # regenerate favicons from assets/kira-mark.png

# The catalogue needs release binaries, so it is a command rather than a target.
# One release directly:
cargo run -p kira-cli -- build --src <dir-of-App/*.uapp> --out site \
    --repo UNAWatch/una-sdk --tag apps-v1.3.0

# ...or several, one directory per tag, with notes from --releases:
#   <src>/apps-v1.3.0/Alarm/Alarm_1.3.0.uapp
#   <src>/apps-v1.2.0/Alarm/Alarm_1.2.0.uapp
cargo run -p kira-cli -- build --src <dir-of-tags> --out site \
    --releases releases.json --repo UNAWatch/una-sdk
```

Use `make serve` rather than opening `site/index.html` directly: the File System
Access API requires a secure context, and `file://` is not one.

`site/data/` and `site/lib/` are generated and git-ignored.

### Tests

`cargo test --workspace` runs everything and needs no network. One integration
test additionally compares two real releases end to end, and skips unless you
point it at them:

```sh
KIRA_FIXTURE_OLD=/path/to/apps-v1.2.0 \
KIRA_FIXTURE_NEW=/path/to/apps-v1.3.0 \
cargo test -p kira-core --test release_diff
```

It cross-checks the planner against hashes it recomputes itself, so it holds for
whichever two releases it is given, with the known 1.2.0-to-1.3.0 split pinned as
a regression check. CI runs it against the two newest releases.

### Icons

`make icons` derives the favicons, apple-touch icon and link-preview card from
`assets/kira-mark.png` using the `image` crate — no ImageMagick. The outputs are
committed, so it only runs when the artwork changes.

The crop geometry is the **measured** bounding box of the mark
(`440x440+484+164`). It is deliberately not an autocrop, because the source render
carries a generator watermark in one corner that a trim would include — the tool
checks the source dimensions and refuses to run if they change, rather than
silently cropping the wrong region.

## Deploying

`.github/workflows/catalog.yml` builds and deploys to Pages on push to `main`,
daily on a schedule, or via *Run workflow* (optionally pinning a specific
`apps-v*` tag or capping how many releases to include). Repository
**Settings → Pages → Source** must be set to **GitHub Actions**.

All asset paths are resolved relative to the page's own URL, so the site works
unchanged at a `github.io/kira/` subpath or on a custom domain.

## Known limitations

- **Chromium desktop only for installing.** Mozilla has filed a negative
  standards position on the File System Access API and WebKit has not shipped it,
  so this gap is not expected to close.
- **The WebAssembly module is ~90 kB gzipped**, which is more than the JavaScript
  it replaced. The trade is one implementation instead of two.
- **No signing.** Integrity is SHA-256 (against this catalogue) plus the `.uapp`
  CRC-32. That protects against corruption and truncation, not against a
  malicious publisher. Kira republishes upstream binaries verbatim and claims no
  review of them.
- **The kernel version cannot be checked over USB.** An app's `minKernelVersion`
  is a BLE/DIS check in the official mobile app. Kira surfaces the LibC ABI
  version from the header instead; matching it to your firmware is up to you.
- **A reboot is required** after installing, and it is not automatable.
- **Release notes are upstream's, not per-app.** A tag's notes cover the whole
  release, so they may describe apps other than the one you are looking at. The
  per-app "did the code change" line is the reliable signal.
- Glance apps are commonly built without icons — their icon fields are present but
  zero-filled — so the catalogue shows a lettered placeholder for them.
- Upstream reassigned the AppIDs of three Glances after `apps-v0.1.9-rc1`, and
  two of its releases ship different apps under one AppID. Kira reports the first
  as separate entries labelled with their IDs, and drops every side of the second
  rather than guessing which binary belongs to which app.
