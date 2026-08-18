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

Built binaries are stored as assets on a long-lived `app-binaries` release, named
`<app>-<version>-<recipe key>.uapp`. That is unrelated to the Actions cache under
Settings, which holds cargo build caches.

`crates/kira-cli/src/recipe.rs` hashes a canonical serialisation of those into a
short key, and `RECIPE_SCHEME` exists so the meaning of a recipe can be changed
explicitly, invalidating every cached artifact instead of silently reusing one
built to older rules.

The flags contribute a *canonical description* (`macro-prefix-map:sdk=…,app=…`)
and not the real arguments, because the real arguments contain absolute paths and
would make the key depend on the build directory, defeating the point.

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
where anything sits. It uses `-fmacro-prefix-map` and not `-ffile-prefix-map`: a
`.uapp` contains no debug info, so only the `__FILE__` macro matters, and leaving
debug paths alone means debuggers still find sources.

A patch adding the same flag to the SDK itself is on
`fix/reproducible-builds-macro-prefix-map`. It is not required for Kira, which
supplies the flag itself, but if upstream takes it, *their* published releases
become reproducible too, and Kira could verify them against source instead of
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

Every app of every published release builds: **169 binaries across 13 releases,
zero failures**, covering all three shapes: Utility and Activity with a GUI
process, Glance without one. The same `Alarm` binary (210,472 bytes, `87217a3a…`)
has come out of separate CI runs days apart.

The store holds several artifacts per version where releases overlap:
`apps-v0.1.9-rc2` and `-rc3` both stamp `0.1.9` while being distinct releases, so
they are distinct recipes and distinct artifacts. That is the recipe key doing its
job.

**A release's tag does not reliably give its version.** `apps-v0.1.9-rc1` ships
binaries stamped `0.1.4`. Since the version is compiled in and therefore part of
the recipe, the build reads it out of upstream's own binary with `kira inspect`
instead of parsing the tag. The stamp is what the catalogue looks up, so it is the
only authority. Deriving it from the tag left ten versions unmatched and falling
back to upstream binaries.

**A variant alias is not built, because it is not compiled.** Upstream packs one
from a manifest, an icon pair and a config JSON (`pack_variants.py` driving
`make_variant.py`), and it has no `*-CMake` project, so nothing in Kira's build
matrix ever looks at it. `Walk` is therefore served as the vendor's
binary with no recipe and no attestation, the same as any version Kira has no
build for, and the card says which of those two it is: *"a variant is packed from
a manifest, not compiled, so there is no source for Kira to build"*. That
is a different statement from a build that was attempted and failed, and it comes
from the binary, the alias flag plus `origin`, and not from a list of variant
names.

Packing them here instead is possible and was weighed. It is the one artifact in
the catalogue that would likely reproduce byte-for-byte, since the packer is
deterministic and every input is in the tagged tree, where no compiled app
reproduces yet. Against that: it means running a Python packer and Pillow inside
a pipeline whose whole premise is a digest-pinned compiler container, and the
resulting attestation would assert little more about 32 bytes of descriptor, two
verbatim PNGs and 58 bytes of JSON than `git show` of the tag already does. Left
undone on those grounds.

Also checked and clean: no `__DATE__`, `__TIME__` or `__TIMESTAMP__` anywhere in
`Libs/`, `Examples/` or `ThirdParty/`; no submodules, so an SDK tag pins its
content; and `file(GLOB_RECURSE)` source lists (22 of them) are safe because CMake
has ordered glob results lexicographically since 3.6, while the apps require 3.21.

## Why the whole history is built, not just recent releases

Serving Kira's build for new releases while republishing upstream's for old ones
would make the changed/unchanged annotation useless at the boundary: comparing
Kira's 1.3.0 against upstream's 1.2.0 says nothing about whether the code changed,
so the build records `changed: null` instead of a false claim. With every release
built the comparison is like-for-like again and the analysis returns: the re-stamp
count over a four-release window goes from 2 back to 8.

## What has not

- **Host architecture.** Everything was built on amd64 runners.
- **A build much later in time.** Nothing has aged.
- **Hostname and username.** Not varied; the container always runs as root.
- **A different toolchain image.** By design: the digest is part of the recipe, so
  a different image is a different artifact, not a reproducibility failure.

## Known weak points

**The toolchain is a personal Docker Hub image**, pinned at
`xanderhendriks/stm32cubeide:16.0@sha256:7e07c508e3944def22eaabe822eaf902a2ba4fbb38a3ce24d6ff874f9f04c447`.
If that account or tag disappears, every reproducibility claim becomes
unverifiable retroactively. Archiving the image somewhere durable is a
prerequisite for relying on any of this publicly.

**Reproducibility is not authenticity.** It shows a binary corresponds to a
source tree. It says nothing about whether that source is benign, and there is no
signing anywhere in this platform: the watch validates a CRC-32, which is a
corruption check. What can be claimed is "built from this source by this recipe,
and the bytes on your watch match what we published", which is provenance and
integrity, not authenticity.

**One part of that is no longer self-referential.** Every `.uapp` uploaded to the
store carries a GitHub build-provenance attestation, signed by the OIDC identity
of the workflow that produced it instead of stored beside the artifact it vouches
for. So a stored binary can be traced to a workflow run and a commit
without taking this repository's word for anything:

```sh
gh attestation verify Alarm-1.3.0-<recipe>.uapp --repo tobymurray/kira
```

That closes the weaker half of the sentence above, the hashes living alongside the
artifacts, and none of the stronger half. An attestation says which workflow
emitted the bytes. It cannot say the source was benign, and it is worth as much as
the account that ran the workflow: an attacker who controls the repository gets
valid attestations for whatever they publish. It raises the cost of a compromise
and makes one visible after the fact, but it is not a signature over reviewed
code, because nothing here reviews code.

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

## The toolchain image

Every binary is compiled in one container, pinned by digest, built from
[`toolchain/Dockerfile`](../toolchain/Dockerfile) and published to
`ghcr.io/tobymurray/kira-toolchain`.

Kira used to point at a third party's image tag directly. Owning it fixes two
things. Reproducibility no longer depends on someone else's tag staying
available: the recipe names a digest in a registry this project controls, and the
Dockerfile that produced it is in this repository. And an app needing a tool the
image lacks can now be accommodated by adding it to the image, rather than
installing it inside the build job, which would leave the recipe's toolchain
digest describing something other than what actually compiled the binary.

It carries the ARM toolchain, CMake, Python for the app packer, and a pinned Rust
with the `thumbv8m.main-none-eabihf` and `thumbv7em-none-eabihf` targets, because
an app's sources are not necessarily all C.

**Changing the image changes every recipe key**, so the next run rebuilds every
artifact under a new identity. That is the intended behaviour: a different
compiler is a different build, and the alternative is a cache that mixes them.

One gap remains. Cargo fetches a crate's dependencies from the network, so they
are not in the image. `Cargo.lock` pins each to an exact version and checksum, so
the fetch is deterministic, but only for an app that commits its lock file. An app
that does not is not reproducible here, whatever the image does.
