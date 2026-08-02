# Kira

**An app store for UNA Watch.** Browse apps, see what's on your watch, and
install or update over the USB cable — from a static web page, with no backend.
Every published version of every app is downloadable, with release notes, and
each version is marked according to whether the app's code actually changed.

> Unofficial. Not affiliated with, endorsed or sponsored by UNA Watch Ltd.
> See [THIRD-PARTY.md](THIRD-PARTY.md).

**Questions, ideas and bugs:** ask in
[Discussions](https://github.com/tobymurray/kira/discussions), file Kira's own
bugs as [issues](https://github.com/tobymurray/kira/issues/new/choose), and report
security problems [privately](https://github.com/tobymurray/kira/security/advisories/new).
Anything about the watch or an app's own behaviour belongs with
[UNA Watch](https://github.com/UNAWatch/una-sdk/issues) instead. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## How it works

There is no server. A GitHub Actions run fetches every published `apps-v*`
release from the [UNA SDK](https://github.com/UNAWatch/una-sdk), reads the
metadata out of each `.uapp` binary, and publishes a catalogue plus the binaries
to GitHub Pages. The page then talks to the watch directly as a USB mass-storage
volume.

Kira builds the binaries it ships rather than trusting upstream's: a second
workflow compiles each app from its tagged source in a digest-pinned ARM
toolchain container and republishes the result, recording what it was built from.
A rebuild that disagrees with upstream about *which app or which version it is*,
or that fails its own CRC, is refused and upstream's artifact used instead. A
rebuild that merely differs byte-for-byte is still what gets served — that is the
expected state until the SDK carries its path-independence fix, and it is
currently true of every SDK app in the catalogue. The card says which binary is
being served and whether it matches the vendor's. See
[docs/reproducibility.md](docs/reproducibility.md).

### One implementation, two runtimes

The interesting logic is Rust. `kira-core` reads the `.uapp` container, models the
catalogue, diffs it against a watch and generates installers; it does no I/O, so
the catalogue build links it natively and the browser loads the very same code
compiled to WebAssembly.

That sharing is not itself the reason for Rust — the JavaScript this replaced was
also one implementation, imported by Node and copied to the browser. What the
rewrite buys is a type system over a packed binary format: `AppId` and `Version`
are newtypes rather than strings, version ordering is the derived `Ord` on a
packed `u32` rather than a hand-written comparison, and `AppType`/`Status` are
enums the compiler checks every match against. It costs about 56 kB gzipped —
see [Module size](#module-size).

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

Kira therefore hashes each version's payload — the icons, the service image and
the GUI image — and compares it with the next older version. That is everything
between the 48-byte header and the CRC footer, so the whole header is out, not
just the version stamp: the AppID, the LibC ABI version, the type and autostart
flags, the display name and the icon lengths are all excluded too. Two builds
that differ only in one of those therefore read as *"same code"*, which is
accurate about the code and silent about the flag. No pair in the catalogue
currently differs that way. Each card says *"code changed in 1.3.0 (+17288 B)"* or *"code unchanged
since 1.2.0"*, and an update that is only a re-stamp is labelled *"version stamp
only, identical code"* rather than presented as new work. That changelog comes
from the binaries, not from prose.

Upstream's own release notes are shown too, per `apps-v*` tag, but not as a wall
of text. A release body is GitHub's generated "What's Changed" list, and most of
it — documentation, the desktop simulator, build tooling — cannot reach a watch.
So each bullet is parsed for its Conventional Commit type and scope and split in
two: changes that ship, and repository churn, collapsed. Each release leads with
what its *binaries* did, which is the part that is not a judgement call.

Nothing is dropped. The split is biased towards "this ships", because wrongly
demoting a real app fix is the harmful error; an unrecognised line is kept as-is,
and the original body stays one click away. The one heuristic that reads a
description rather than its scope catches simulator work the SDK files under an
app's name, e.g. `fix(hrmonitor): make the GCC/Linux simulator build`.

Every string is rendered as **text, never as HTML** — it is third-party Markdown,
and it is not going anywhere near `innerHTML`.

Any published version can be selected per app; the newest is the default.
Selecting an older one re-targets both the download and the installer, so a watch
already on the newest build correctly reports `newer-on-watch` rather than being
silently downgraded.

### Two capability tiers

|                            | Chrome / Edge / Opera | Firefox / Safari |
| -------------------------- | --------------------- | ---------------- |
| Browse the catalogue       | yes                   | yes              |
| Read what's on the watch   | yes                   | yes              |
| Install and update         | yes, in-page          | generated script |
| Fill in an app's settings  | yes, in-page          | no               |
| Remember the chosen folder | yes                   | no               |

Writing to a removable drive needs the File System Access API, which only
Chromium desktop implements. Reading uses `<input webkitdirectory>`, which is
supported nearly everywhere — so Firefox and Safari still get the full inventory
and version diff, then a ready-to-run PowerShell or `sh` script that performs
exactly the writes the page planned. Which of the two is offered follows the
visitor's platform, and either one locates the watch by its volume label rather
than by a path, since where a USB drive appears is stable on no platform.

The chosen directory handle is kept in IndexedDB, so a reload does not mean
picking the watch again. The *permission* to use that handle is a separate thing
and Chromium drops it once every tab on the origin closes, so a lapsed one becomes
a one-click **Reconnect** button — re-granting needs a user gesture, and page load
is not one. Installing Kira (the manifest exists for this reason, not for looks)
lets Chromium offer to keep the permission across visits, which removes the click
too. Firefox and Safari read the drive through `<input webkitdirectory>`, which
yields files rather than a handle, so there is nothing to persist and the folder
has to be picked each visit.

Settings have no script fallback, unlike installing, and that is a deliberate
refusal rather than an omission. The installers reference files by name; a
setting is a value the visitor types, which would have to be *embedded* in the
generated shell and PowerShell — a sink that does not exist there today, in a
file people are told to run on their own machine. Getting shell quoting, JSON
escaping and their interaction right in two languages is not worth it for a
convenience feature, so on Firefox and Safari the form says which file to type by
hand instead. Commit `ad4482a` is why: a display name read out of a binary
reached past a `#` comment and into a live statement in both installers.

### Settings an app reads

Some apps need a value only their owner knows — an athlete id, a transit pass, an
account token. The watch has four buttons and no keyboard and the SDK offers no
way to send one in, so the app reads it from a file in its own folder and Kira
fills that file in over the same USB handle it installs through.

**The app owns the format entirely.** Its manifest declares the file name, the
schema number and every key; Kira assembles the document, refuses values the app
could not read back, and writes it. Nothing about the convention is Kira's, which
matters because it is nobody's standard yet: the SDK ships `SDK::Variant::Config`
with exactly this shape — an exact `schema` major, an app-owned subtree the reader
treats as opaque, a size ceiling checked before allocating, defaults on every
failure — but only for configs the platform itself writes. See
[UNAWatch/una-sdk#225](https://github.com/UNAWatch/una-sdk/issues/225).

This is the one thing on a card that **cannot come from the binary**. Nothing in a
`.uapp` says what it reads, so it is the submitter's assertion — and the only
assertion Kira acts on rather than merely renders, since it names a file written
to somebody's watch. It is checked on every catalogue build, not just at review:
the name must be a plain file in the app's own folder, must not look like an app
binary, and every key must be dot-separated plain segments.

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

### Reproducible builds

Almost every app Kira serves is built from source by a recipe that pins the SDK
revision, the toolchain container digest, the stamped version and the flags. The
exceptions carry `origin: "upstream"` — a version Kira has no build for, or whose
build was refused — and the card calls those the vendor's own build. See
[docs/reproducibility.md](docs/reproducibility.md) for what that rests on, what
has been verified, and the weak points — including the fact that reproducibility
is not authenticity.

The SDK revision in a recipe is a tag, and tags move — which `registry/README.md`
tells submitters is exactly what makes a pinned source a lie. Changing the recipe
to name a commit would invalidate every stored artifact and force a full rebuild,
so instead `sdk-tags.lock.json` records the commit each published tag pointed at
and the catalogue build refuses to publish if upstream has moved one. That does
not make the recipe immutable; it makes a move impossible to miss.

### Module size

The published `.wasm` is ~78 kB gzipped:

| | gzipped |
| --- | --- |
| scripts before the Rust rewrite (`app.js` + three shared modules) | 18.5 kB |
| the module at the time of the rewrite | 58.2 kB |
| ...plus release notes, submissions and per-version notes since | 72.2 kB |
| ...plus the settings form | 79.7 kB |
| `catalog.json` (13 releases, 151 versions) | 20.6 kB |
| one app install, e.g. Running 1.3.0 | 520 kB |

The first two rows are the rewrite's own measurement, kept because the argument
below is about it. The rest is drift since, measured the same way — the module
has grown 21.5 kB across four features and nobody was watching, which is the
usual way a budget goes.

So the rewrite costs ~56 kB on first load, against a page that already transfers
20 kB of catalogue and, the moment anyone does the thing the site exists for,
half a megabyte per app. It is cached after the first visit. On a slow connection
the first load is nonetheless noticeably heavier, and that is the honest cost.

Two dependencies were worth removing, each measured rather than guessed:

| Change | gzipped |
| --- | --- |
| baseline | 90.0 kB |
| drop `serde_json` — the browser already has a JSON parser, so `Store::new` takes the result of `JSON.parse` | 73.9 kB |
| drop `crc32fast` for a `const` table — it ships SIMD dispatch and multi-kilobyte slice-by-N tables that compress poorly | 58.2 kB |

**`wasm-opt` is deliberately not used.** It shrinks the raw module by ~11%
(152 kB → 135 kB) but makes it *less compressible*, so the gzipped transfer grows
to ~60 kB. Raw size still matters for parse time and memory, so it is a real
trade — just not the one that helps a visitor on a network. Measure gzipped
output before adding it.

What remains is mostly unavoidable: ~12 kB of float-to-decimal formatting pulled
in by serde's error machinery, ~5 kB of allocator, and ~10 kB of generated
deserializers. Removing an unused `sha2` dependency from `kira-core` changed
nothing measurable, since LTO was already discarding it.

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
unchanged at a `github.io/kira/` subpath or on a custom domain. The domain lives
in `site/CNAME` as well as the repository settings, so a deploy cannot drop it.

**The custom domain must serve HTTPS.** The File System Access API requires a
secure context, so on plain HTTP the install path silently disappears and every
visitor gets the read-only tier. Enforce HTTPS in Settings → Pages once GitHub has
issued the certificate.

## Known limitations

- **Chromium desktop only for installing.** Mozilla has filed a negative
  standards position on the File System Access API and WebKit has not shipped it,
  so this gap is not expected to close.
- **The WebAssembly module is ~58 kB gzipped** (152 kB raw), which is more than
  the JavaScript it replaced. The trade is one implementation instead of two.
  See [Module size](#module-size) before trying to shrink it further.
- **No signing of the apps themselves.** Integrity is SHA-256 (against this
  catalogue) plus the `.uapp` CRC-32, which catches corruption and truncation,
  not a malicious publisher. Every binary in the store does carry a GitHub
  build-provenance attestation — `gh attestation verify <file> --repo
  tobymurray/kira` — so it can be tied to the workflow run that produced it
  without trusting this repository. That is still not authenticity: it says which
  build emitted the bytes, not that the source was benign, and nothing here
  reviews code.
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
