# Submitting an app

Kira carries apps that are not UNA's. What you submit is **a pointer to source**,
not a binary: a repository, a commit, and the SDK revision to build against. Kira
then compiles it itself, in the same digest-pinned toolchain container it uses for
every other app in the catalogue.

That is not bureaucracy — it is the only thing that makes the catalogue checkable.
The watch has no code signing, and it silently ignores a `.uapp` whose CRC fails,
so "trust me, this binary is fine" is not something anyone can verify. A binary
built from a named commit by a published recipe is.

## What you need first

- **A public repository** with your app's source, under an open licence.
- **An app that builds against the SDK** — it needs the usual layout, a
  `Software/<name>-CMake/CMakeLists.txt` declaring `APP_ID`, `APP_TYPE` and
  `APP_NAME`. If `kira build-app` can build it, Kira can carry it.
- **An `AppID` nobody else uses.** It is 64 bits and it is the app's whole
  identity on the watch — the folder name and the display name are not. Generate
  one at random rather than picking something memorable.

## The manifest

Add one file, `registry/<slug>.toml`:

```toml
app_id    = "A7C31F0E9B482D65"                          # 16 hex digits
source    = "https://github.com/someone/una-tide-clock"
subdir    = "."                                          # optional, defaults to the repo root
folder    = "TideClock"                                  # where it installs under Apps\
licence   = "MIT"
maintainer = "someone"                                   # GitHub handle

[[versions]]
version = "1.0.0"
rev     = "3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d"     # full commit sha
sdk_rev = "apps-v1.3.0"                                  # SDK release to build against
notes   = "What changed in this version."                # optional, shown on the card
# subdir = "old/path"                                    # optional, if this version lived elsewhere
```

Then open a pull request. Check it locally first — the same checks CI runs:

```sh
cargo run -p kira-cli -- registry validate --catalog site/data/catalog.json
cargo run -p kira-cli -- registry plan --toolchain unpinned
```

## If your app reads a settings file

A watch with four buttons and no keyboard cannot be told an athlete id, a transit
pass or an account token, and the SDK offers no way to send one in. If your app
reads such a value from a file in its own folder, say so and the page will offer
a form that writes it over USB:

```toml
[config]
file   = "input.json"     # written into Apps/<folder>/ — a plain name, never a path
schema = 1                # the document's top-level "schema", for apps that gate on it

[[config.fields]]
path      = "values.id"   # where the value goes, dot-separated
title     = "Athlete id"  # the label
help      = "The characters printed under the barcode on your parkrun card."
maxLength = 16
required  = true          # optional: the app does nothing useful without it
```

`required` changes how the page presents a field, not what gets written. A
required field is shown above the download instead of in a collapsed panel
underneath it, and once a watch is connected the card says when the value is not
on it yet — because an install that worked and a value nobody wrote look
identical otherwise, which is how somebody ends up holding an app that does
nothing and no way to tell why. Leave it off when the app is fine either way:
`Barcode` scans as nothing without an id, but `Squash` records a perfectly good
activity with its IMU flag untouched, and nagging about that would be wrong.

It is not enforced. Like everything else in this block it is your word and cannot
be checked against a binary, so nothing refuses an install over it — somebody may
want the `.uapp` now and the value later, or may write the file by hand.

which produces

```json
{
  "schema": 1,
  "values": {
    "id": "A1234567"
  }
}
```

**You own the format; Kira only assembles it.** The file name, the schema number
and every key come from here, so nothing about the convention is Kira's to
change. Add a field and the form grows a row.

Worth knowing before you rely on it:

- **This is the one thing on a card that cannot come from your binary.** Nothing
  in a `.uapp` says what it reads, so it is your word for it — and unlike
  `notes`, which is only displayed, this is *acted on*: it names a file written
  to somebody's watch. Expect the checks to be fussier than the shape needs, and
  expect them to run on every catalogue build rather than only on your pull
  request.
- **Values are restricted to printable ASCII without `\` or `"`.** Not JSON's
  rule — a reader built on coreJSON gets the raw slice with escapes undecoded, so
  an escaped character would reach your app as the literal characters of its
  escape sequence. A value that cannot survive the trip is refused in the form,
  where there is somewhere to explain why.
- **Over-long values are refused, never trimmed.** A shortened id is a wrong id.
- **Validate again on the watch.** The form is a convenience; the file is a text
  file on a mass-storage volume that anyone can edit with Notepad. Bound the read
  before you allocate, gate on the schema number, and fall back to a default
  rather than failing to start — `SDK::Variant::Config` in the SDK is the
  reference for all three.
- **Chromium desktop only**, like installing. The generated scripts carry no
  settings, deliberately: see the README.

## One repository or several?

**One repository holding all your apps is the better default.** `subdir` exists for
exactly this: one manifest per app, each pointing at the same `source` with a
different path. Versions stay independent — a manifest is only rebuilt when its own
entries change — so a monorepo does not force your apps into lockstep, and shared
helpers are just a directory rather than a submodule.

```
una-apps/                      registry/tide-clock.toml  → subdir = "tide-clock"
├── shared/                    registry/tide-glance.toml → subdir = "tide-glance"
├── tide-clock/
│   └── Software/TideClock-CMake/CMakeLists.txt
└── tide-glance/
    └── Software/App/TideGlance-CMake/CMakeLists.txt
```

The one hard constraint: **each app directory must contain exactly one
`*-CMake` project** under `Software/`. Kira refuses to guess between two, so apps
cannot share a `Software/` tree.

Reach for separate repositories when an app has a different licence, a different
set of maintainers, or an audience that should not have to clone the rest.

If you later rearrange the repository, do not edit `subdir` on versions already
published — the path is part of the recipe, so that would change what those
versions claim to be built from. Set `subdir` on the new version instead, and pin
the old ones to where they actually lived.

## What gets checked, and why

| Rule | Reason |
| --- | --- |
| `rev` is a full 40-character commit sha | Tags and branches move. A "pinned" source that can change makes the published recipe a lie. |
| `app_id` is unused | It is the app's only identity. Two apps sharing one confuses the watch, the phone app and this catalogue at once. |
| `folder` is unused, and is a name FAT accepts | The watch loads whichever `.uapp` it finds first in a folder, so a collision can silently boot the wrong app. |
| `source` is a plain `https` URL | It is published in the catalogue and fetched by CI. No credentials, no query string. |
| `subdir` stays inside the repository | It is a path handed to a build. |
| A published version's `subdir` never changes | The path is part of the recipe. Moving an app is fine; rewriting where an old version came from is not. |
| `licence` is a recognised open licence | Source-accessible is the premise. If yours is missing from the list, add it in the same pull request. |
| A published version's `rev` never changes | Somebody's watch may be carrying it. Change anything by publishing a new version. |
| A manifest is retired, not deleted, once published | An app that vanishes leaves every watch carrying it holding something the catalogue cannot name. A submission that never reached the catalogue can simply be withdrawn. |
| `config.file` is a plain name in the app's own folder, and not a `.uapp` | It is a path the page writes to a device. A name that escaped the folder would write anywhere on the volume; one ending in `.uapp` could be the file the watch boots. |
| Every `config.fields[].path` is dot-separated plain segments, unique, and not nested inside another | The keys go straight into a document the app parses. Two fields writing the same place, or one inside another, cannot both be satisfied. |

CI then fetches exactly that commit, builds it, and checks the result against what
your own `CMakeLists.txt` declares: `AppID`, type, version, and the `.uapp`'s CRC.
A build whose binary disagrees with its source fails.

Pull requests run with no secrets and no write access, and **nothing is published
from a pull request**. Merging to `main` is what publishes: the same build runs
again, its binaries go to the content-addressed store, and the catalogue picks
them up from there.

Every rule in that table is checked again on **each** catalogue build, not only
when your pull request was reviewed. That is not distrust — a manifest is only
ever checked against the catalogue as it stood at the time, and upstream can ship
a colliding `AppID` or folder in any later release. A collision that appears
afterwards stops the whole catalogue build rather than reaching a watch.

The three rules about what a *published* version may no longer change are checked
twice, in two different ways, because they are the ones a merge can slip past. Your
pull request is compared against the commit it branched from, which catches the
ordinary edit. The catalogue build then compares every manifest against the
catalogue it is currently serving — the `builtFrom` recorded on each published
version, which is the recipe the binary on somebody's watch actually came from.
The second check needs no git history, so it holds however a manifest reached
`main`: a direct push, or a branch whose own review compared it against a base that
already carried the change. If the published source and the manifest disagree,
nothing is published until they are reconciled.

## Things worth knowing

- **Kira does not review your code**, and says so. What the catalogue offers is
  provenance and integrity — that a binary was built from a named commit by a
  published recipe, and that the bytes on your watch match what was published. It
  cannot tell anyone whether the app is any good or safe. See
  [SECURITY.md](../SECURITY.md).
- **Your app is listed with UNA's**, in the same grid for its type, in the same
  order, with no badge marking it out. Kira has no way to judge whether an app is
  any good, so a layout implying a ranking would be claiming something it does not
  know. What the card does say is where the binary came from — *built by Kira from
  `<your repo>` at `<commit>`* against *the vendor's own build* — which is
  information, not a verdict.
- **Updating** means a new `[[versions]]` entry with a new commit. Old versions
  stay: every published version of every app remains downloadable.
- **`notes` is how a version says what changed**, in 4 to 500 characters, shown on
  the card. Nothing else can: the release notes panel carries the UNA SDK's own
  release bodies, and none of them mention your app. It is the one line on a card
  that nobody can check — it sits below the byte-derived history, which says
  whether the code actually moved, and above the pinned commit, which is the
  account of record. Alone among a published version's fields it stays editable,
  because it is not part of the recipe: correcting a typo changes no artifact.
- **Taking a listing down** is `retired`, not a deletion:

  ```toml
  retired = "the sensor it reads was removed in firmware 2.0"   # the whole app

  [[versions]]
  version = "1.0.0"
  retired = "writes a corrupt .fit on runs over an hour"        # just this one
  ```

  A retired app moves to the collapsed archive at the foot of the catalogue,
  keeps its binaries, and is never offered for installation. Retiring a single
  version leaves the app where it is and marks that version alone. Either way the
  reason is shown on the card, because a watch already carrying it should be
  recognised and told why, which is more use than being reported as something
  unknown. The reason is required: a bare flag tells that person nothing.
  Deleting a manifest outright is reserved for cases where the binaries must
  genuinely stop being served — see
  [Getting an app removed](../SECURITY.md#getting-an-app-removed) for what those
  are, how to ask, and what removal can and cannot do.
- **Versions are yours to number.** Unlike UNA's apps, which are all stamped with
  the release tag they shipped in, a submission's version means whatever you
  intend it to mean.

Questions before you start are welcome in
[Discussions](https://github.com/tobymurray/kira/discussions/new?category=q-a) —
better than finding out in review that the app cannot be built.
