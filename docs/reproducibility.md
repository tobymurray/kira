# Reproducible builds

Kira builds every app it serves from source, and the build is reproducible: given
the same recipe, anyone gets the same bytes. This records what that rests on, and
what it does *not* cover, so nobody has to re-derive it.

## The recipe

A `.uapp` is a function of more than its source. All of these change the output,
so all of them are part of the recipe that keys a cached artifact:

| input | why it matters |
| --- | --- |
| app source (repo + revision, or SDK tag + path) | the obvious one |
| SDK revision | the app links against it |
| toolchain container **digest** | a different compiler is a different binary |
| `BUILD_VERSION` | compiled in as a string, so it changes `.rodata` |
| flags Kira adds | see below |

`crates/kira-cli/src/recipe.rs` hashes a canonical serialisation of those into a
short key, and `RECIPE_SCHEME` exists so the meaning of a recipe can be changed
deliberately, invalidating every cached artifact rather than silently reusing one
built to older rules.

The flags contribute a *canonical description* (`macro-prefix-map:sdk=…,app=…`)
rather than the real arguments, because the real arguments contain absolute paths
and would make the key depend on the build directory — defeating the point.

## The build path problem

`Libs/Header/SDK/UnaLogger/Logger.h` defines `__FILENAME__` as a **runtime**
`strrchr` over `__FILE__`, so the full path of every SDK source that logs is
embedded in `.rodata` and ends up inside the `.uapp`. Since SDK sources compile
through absolute paths under `UNA_SDK`, the binary depends on where the SDK is
checked out.

Measured on `Alarm` from `apps-v1.3.0`, varying only the SDK checkout path:

- 210,488 bytes at one path, 210,932 at another — **444 bytes apart**
- the longer checkout path was present verbatim in the binary

Kira passes `-fmacro-prefix-map` for both trees, so its builds do not depend on
where anything sits. `-fmacro-prefix-map` rather than `-ffile-prefix-map`: a
`.uapp` contains no debug info, so only the `__FILE__` macro matters, and leaving
debug paths alone means debuggers still find sources.

A patch adding the same flag to the SDK itself is on
`fix/reproducible-builds-macro-prefix-map`. It is not required for Kira — Kira
supplies the flag itself — but if upstream takes it, *their* published releases
become reproducible too, and Kira could verify them against source rather than
merely recording their hashes.

## What has been verified

`Alarm` from `apps-v1.3.0`, in the pinned container, byte-identical while varying
one factor at a time:

- SDK checkout path
- app checkout path
- CMake generator (Unix Makefiles and Ninja)
- parallelism (`-j1` and `-j$(nproc)`)
- locale, timezone, umask, `SOURCE_DATE_EPOCH`

An independent scan confirmed **no build path appears in any output**, which does
not depend on trusting that the prefix maps covered everything.

All 13 apps of `apps-v1.3.0` build standalone and pass verification, across all
three shapes: Utility and Activity with a GUI process, Glance without one. The
same `Alarm` binary — 210,472 bytes, `87217a3a…` — has come out of separate CI
runs days apart.

Also checked and clean: no `__DATE__`, `__TIME__` or `__TIMESTAMP__` anywhere in
`Libs/`, `Examples/` or `ThirdParty/`; no submodules, so an SDK tag pins its
content; and `file(GLOB_RECURSE)` source lists (22 of them) are safe because CMake
has ordered glob results lexicographically since 3.6, while the apps require 3.21.

## What has not

- **Host architecture.** Everything was built on amd64 runners.
- **A build much later in time.** Nothing has aged.
- **Hostname and username.** Not varied; the container always runs as root.
- **A different toolchain image.** By design — the digest is part of the recipe,
  so a different image is a different artifact rather than a reproducibility
  failure.

## Known weak points

**The toolchain is a personal Docker Hub image**, pinned at
`xanderhendriks/stm32cubeide:16.0@sha256:7e07c508e3944def22eaabe822eaf902a2ba4fbb38a3ce24d6ff874f9f04c447`.
If that account or tag disappears, every reproducibility claim becomes
unverifiable retroactively. Archiving the image somewhere durable is a
prerequisite for relying on any of this publicly.

**Reproducibility is not authenticity.** It shows a binary corresponds to a
source tree. It says nothing about whether that source is benign, and there is no
signing anywhere in this platform — the watch validates a CRC-32, which is a
corruption check. Kira's hashes also live in the same repository as the artifacts
they describe, so they cannot survive a compromised publisher. The honest claim is
"built from this source by this recipe, and the bytes on your watch match what we
published" — provenance and integrity, not authenticity.

**Verification requires a rebuild**: the exact container digest, SDK revision and
version string, and several minutes. Realistically a third party does that once,
not every user. That is also how F-Droid works in practice.

## Reproducing a published binary

```sh
kira build-app \
  --app <app source tree> \
  --sdk <SDK checkout at the recorded revision> \
  --version <the recorded BUILD_VERSION> \
  --toolchain <the recorded container digest> \
  --sdk-rev <the recorded SDK revision> \
  --app-source <the recorded app source identity> \
  --out ./rebuilt.uapp
```

Run it inside that container image, then compare `sha256sum` against the
`sha256` the catalogue records for that version. Every input above is published
in the catalogue's `builtFrom` for exactly this purpose.
