# Kira

**An app store for UNA Watch.** Browse apps, see what's on your watch, and
install or update over the USB cable — from a static web page, with no backend.

> Unofficial. Not affiliated with, endorsed or sponsored by UNA Watch Ltd.
> See [THIRD-PARTY.md](THIRD-PARTY.md).

## How it works

There is no server. A GitHub Actions run fetches the newest `apps-v*` release
from the [UNA SDK](https://github.com/UNAWatch/una-sdk), reads the metadata out
of each `.uapp` binary, and publishes a catalogue plus the binaries to GitHub
Pages. The page then talks to the watch directly as a USB mass-storage volume.

Apps are never rebuilt here — the SDK's own CI already builds every app in its
ARM toolchain container, so Kira consumes those artifacts.

### Everything comes from the binary

A `.uapp` is self-describing: a 48-byte header carries the AppID, version, the
LibC ABI version, type and autostart flags, the display name, and two embedded
icons. Kira parses that (see [`src/uapp.js`](src/uapp.js)) both at build time and
in the browser, so the catalogue and the installed-app inventory use one parser
with no second implementation to drift.

The catalogue is keyed on **AppID**, never on folder or display name. Folder
names are arbitrary, and display names can contain a path separator — the
`GlanceARHR` app is really named `AVG / R HR`.

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
   failing CRC is dropped *silently* by the watch kernel, so the app would
   simply never appear.
2. Write the new `.uapp` into `Apps/<Folder>/`.
3. Read it back and check the length.
4. Only then delete any stale `.uapp` in that folder. The watch loads whichever
   it finds first, so leaving two can keep booting the old build.

App folders are never removed, so each app's `settings.json` and `Activity/`
data survive an update. Apps on the watch that aren't in the catalogue are
reported and left alone.

**Verification is a two-step affair.** Hashing straight after writing reads the
OS write cache and can report a false OK. Eject the watch, reconnect it, then
press *Verify flash* — Kira keeps the directory handle in IndexedDB so it can
re-check without you re-picking the drive. Then reboot the watch: the launcher
list is rebuilt only at boot.

## Development

```sh
npm test                       # 35 unit tests, no network, no dependencies

# Build the catalogue from a release you already have unzipped:
node tools/build-catalog.mjs --src <dir-of-App/*.uapp> --out site \
    --repo UNAWatch/una-sdk --tag apps-v1.3.0

npm run serve                  # http://localhost:8099
```

Use `npm run serve` rather than opening `site/index.html` directly: the File
System Access API requires a secure context, and `file://` is not one.

`site/data/` and `site/lib/` are generated and git-ignored — `build-catalog.mjs`
writes the catalogue and copies the shared ES modules into `site/lib/`.

### Layout

```
src/uapp.js              .uapp parser, CRC-32, ABGR2222 icon decode  (shared)
src/plan.js              catalogue-vs-watch diff, install script generation (shared)
src/png.js               dependency-free RGBA -> PNG encoder (build only)
tools/build-catalog.mjs  release dir -> site/data/{catalog.json,icons,apps} + site/lib
tools/serve.mjs          local static server
site/                    the published page
```

## Deploying

`.github/workflows/catalog.yml` builds and deploys to Pages on push to `main`,
daily on a schedule, or via *Run workflow* (optionally pinning a specific
`apps-v*` tag). Repository **Settings → Pages → Source** must be set to
**GitHub Actions**.

All asset paths are resolved relative to the page's own URL, so the site works
unchanged at a `github.io/kira/` subpath or on a custom domain.

## Known limitations

- **Chromium desktop only for installing.** Mozilla has filed a negative
  standards position on the File System Access API and WebKit has not shipped
  it, so this gap is not expected to close.
- **No signing.** Integrity is SHA-256 (against this catalogue) plus the `.uapp`
  CRC-32. That protects against corruption and truncation, not against a
  malicious publisher. Kira republishes upstream binaries verbatim and claims no
  review of them.
- **The kernel version cannot be checked over USB.** An app's `minKernelVersion`
  is a BLE/DIS check in the official mobile app. Kira surfaces the LibC ABI
  version from the header instead; matching it to your firmware is up to you.
- **A reboot is required** after installing, and it is not automatable.
- Glance apps are commonly built without icons — their icon fields are present
  but zero-filled — so the catalogue shows a lettered placeholder for them.
