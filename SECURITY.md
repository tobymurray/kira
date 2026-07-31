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

## What to expect

This is one person's side project, so there is no response-time guarantee. A
report that includes the app, version, browser and what you observed will be acted
on considerably faster than one that does not.
