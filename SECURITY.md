# Reporting a security problem

**Do not open a public issue or discussion for a security problem.** Use GitHub's
[private vulnerability reporting](https://github.com/tobymurray/kira/security/advisories/new)
instead, which is visible only to the maintainer until it is resolved.

Kira writes executable binaries to a device over USB, and the platform has no code
signing — the watch validates a CRC-32, which detects corruption, not tampering.
So the consequences of a flaw here can reach hardware, and a public report would
tell everyone else how to exploit it before it can be fixed.

## In scope

- Anything that could cause Kira to write the **wrong binary** to a watch, or to
  write to somewhere it should not.
- Ways to make the catalogue misreport what an app **is** — its identity, version,
  provenance, or the hash it claims.
- Ways to defeat the checks before an install: the SHA-256 against the catalogue,
  the `.uapp` CRC-32, or the `AppID` and version cross-check.
- Anything that could make the published site execute code it did not ship, or
  reach the File System Access handle from outside Kira's own script.
- Problems in the build pipeline that would let an artifact enter the store
  without having been built from the source it claims.

## Out of scope here

- **Bugs in the apps themselves, or in the watch firmware.** Kira publishes UNA's
  apps and builds them from UNA's source; it does not write them. Report those to
  [UNA Watch](https://github.com/UNAWatch/una-sdk/issues).
- Lost or corrupted activity data caused by the watch or an app rather than by
  Kira writing to the wrong place.
- Missing protections Kira never claimed. In particular Kira offers **provenance
  and integrity, not authenticity**: it can show that a binary was built from a
  given source by a published recipe, and that the bytes on your watch match what
  was published. It cannot prove who wrote the source, and its hashes live in the
  same repository as the artifacts they describe, so they do not survive a
  compromised publisher. See [docs/reproducibility.md](docs/reproducibility.md).
- **A value typed into an app's Setup form is stored in the clear.** It is written
  to a plain file on the watch's USB volume, readable by anything on any computer
  the watch is plugged into and by any other app on it. There is nowhere else to
  put it: the app reads it back with a bounded JSON parser on a device with four
  buttons and no keystore. So "the settings file is not encrypted" is the design
  rather than a flaw in it — but if you find a way to read one *without* physical
  access to the watch, that is very much in scope.

## What to expect

This is one person's side project, so there is no response-time guarantee. A
report that includes the app, version, browser and what you observed will be acted
on considerably faster than one that does not.

# Getting an app removed

Separate from the above, because most reasons to want an app gone are not
security problems: a licence you did not agree to, an app that damages the watch,
or simply being its author and wanting it delisted.

## Where to send it

- **Malicious, or a security problem** — use the
  [private advisory](https://github.com/tobymurray/kira/security/advisories/new)
  form above, so nobody learns how to exploit it before it is fixed.
- **Anything else** — open a public
  [issue](https://github.com/tobymurray/kira/issues/new/choose). There is nothing
  to keep quiet about a licence complaint or a broken app, and in the open it
  stays visible if I am slow.
- **If I do not respond and it matters** — report it to GitHub. The binaries are
  release assets and the site is GitHub Pages, so GitHub can act on this
  repository without me. That route exists precisely because the one below does
  not scale.

## What removal actually does

Two mechanisms, and the milder one is the default.

**Retiring** is the normal answer. The app stays listed in the archive at the
foot of the catalogue, keeps its binaries, is never offered for installation
again, and the reason is shown on its card. A watch already carrying it is then
recognised and its owner told why — which is more use to that person than the app
silently becoming something the catalogue cannot name. It takes effect on the
next catalogue build.

**Deleting** removes the manifest and stops the binaries being served at all. It
is reserved for when the bytes must genuinely stop being available: malware, a
licence that gives no right to distribute them, or a legal demand. It is worse
for anyone already carrying the app, which is why it is not the default.

## What removal cannot do

**It cannot take anything off a watch.** Kira never deletes an app folder — that
is deliberate, so `settings.json` and `Activity/` survive an update — and it has
no way to reach a device that is not plugged in and driving the page. Removal
stops *new* installs and explains the situation to anyone who connects. F-Droid,
Homebrew and crates.io are all the same in this respect: an index can stop
offering something, it cannot reach into machines that already took it.

It also cannot reach a binary somebody already downloaded.

## What would get something removed

- It is malicious, or does something it did not disclose.
- It damages the watch, its storage, or recorded activity data.
- There is no right to distribute it — the licence does not allow it, or the
  author has asked for it to go.
- Its `AppID` or on-device folder collides with another app in a way that could
  boot the wrong binary.
- Its source has disappeared, so the build can no longer be checked by anyone.
  That is grounds for retiring rather than deleting: the binary is not suddenly
  unsafe, it has just stopped being verifiable.

## How quickly

There is no answer to this that would be honest and also reassuring. This is one
person's side project; **nobody else holds the keys.** Something obviously
dangerous will be retired as soon as it is read, which is a matter of hours or
days depending entirely on what else is happening. Nothing here is on call.

If that is not good enough for your situation — and for a genuinely malicious app
it should not be — use the GitHub route above rather than waiting on me.

## If I disappear

The registry is a directory of plain TOML files in a public repository, the site
is static, and the whole catalogue is rebuilt from source by workflows anybody can
read. So anyone can fork it and carry on, and GitHub can take this copy down on
report. What does not survive me is the speed of response, not the ability to
respond at all — that is the honest shape of the bus factor here, and the reason
nothing in Kira's design asks you to trust that I am available.
