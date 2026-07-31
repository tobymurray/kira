# Contributing

Kira is an unofficial app store for UNA Watch, and **not a support channel for
the watch itself**. If the watch or an app misbehaves once installed, that
belongs with [UNA Watch](https://github.com/UNAWatch/una-sdk/issues) — Kira only
distributes their apps.

## Where to go

| You want to | Go here |
| --- | --- |
| Ask how something works | [Discussions → Q&A](https://github.com/tobymurray/kira/discussions/new?category=q-a) |
| Suggest a change, or an app worth carrying | [Discussions → Ideas](https://github.com/tobymurray/kira/discussions/new?category=ideas) |
| Report a bug in Kira | [New issue](https://github.com/tobymurray/kira/issues/new/choose) |
| Report a security problem | [Privately](https://github.com/tobymurray/kira/security/advisories/new) — see [SECURITY.md](SECURITY.md) |
| Show what you built | [Discussions → Show and tell](https://github.com/tobymurray/kira/discussions/new?category=show-and-tell) |

Questions go to Discussions rather than issues on purpose: the answer stays
searchable for the next person who asks the same thing, and the issue tracker
stays a list of things that are actually broken.

Nothing posted in Discussions or issues is rendered on the site. The published
page loads no third-party script, because it holds a read/write handle to your
watch while you use it.

## Reporting a bug well

The [bug form](.github/ISSUE_TEMPLATE/bug.yml) asks for browser, operating
system, app and version, because most reports are unactionable without them.
Two things worth knowing before you file:

- **Installing needs a Chromium desktop browser.** Firefox and Safari have not
  implemented the directory-write half of the File System Access API, so they get
  a generated install script instead. That is the intended behaviour, not a bug.
- **A binary that doesn't match the catalogue is worth reporting.** If a card says
  the bytes on your watch differ from what Kira published, include the hashes the
  page shows.

## Working on the code

```sh
make check    # fmt, clippy, tests
make wasm     # build the wasm module into site/
make serve    # build the wasm module, then serve site/ on :8099
```

The workspace is three crates: `kira-core` holds the `.uapp` parser, catalogue
model and install planner with no I/O at all; `kira-cli` is the build-time tool;
`kira-wasm` is the thin browser binding. Logic belongs in `kira-core` so that the
CLI and the page cannot disagree — if you find yourself writing a rule twice,
that is the bug.

`site/app.js` is deliberately dumb: DOM and File System Access plumbing only. No
parsing, no version comparison, no decisions about what to install.

Run `make check` before opening a pull request. New behaviour in `kira-core`
wants a test; there is no test harness for the browser layer, so changes there
are verified by hand against a real watch.

## Adding an app

Kira builds every binary it ships from source, so an app has to be buildable and
its source has to be public — see [docs/reproducibility.md](docs/reproducibility.md)
for what "reproducible" does and does not mean here. Submissions of third-party
apps are not open yet; if you have one you want carried,
[open an idea](https://github.com/tobymurray/kira/discussions/new?category=ideas).

## Licence

Kira's own code is MIT. App binaries are redistributed from the UNA SDK's public
releases under its MIT licence — see [THIRD-PARTY.md](THIRD-PARTY.md).
