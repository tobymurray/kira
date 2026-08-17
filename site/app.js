/**
 * Kira — browser front end.
 *
 * Only what a browser must do itself lives here: the File System Access API,
 * IndexedDB, and the DOM. Everything about the `.uapp` format, version
 * selection, diffing and installer generation comes from the same Rust the
 * catalogue build uses, compiled to WebAssembly — see crates/kira-wasm.
 *
 * Two capability tiers, because writing to a removable drive from a page is
 * Chromium-only:
 *
 *   write mode  showDirectoryPicker() — read the watch and install directly.
 *   read mode   <input webkitdirectory> — read the watch (works in Firefox and
 *               Safari too) and hand back a generated install script.
 *
 * Install ordering follows Update-Watch-Apps.ps1 from the UNA SDK: write the new
 * binary, verify it, and only then delete the stale one. App folders are never
 * removed, so settings.json and Activity/ survive an update.
 */

/**
 * Bound by loadModule() once the schema the catalogue is published at is known.
 *
 * Deliberately not a static import: see loadModule() for why the module has to
 * be fetched after the catalogue rather than alongside it.
 */
let Store;
let crcIsValid;
let payloadBounds;
let readHeader;
let readVariant;
let sourceRef;

/**
 * Load the WebAssembly module, keyed to the schema it has to understand.
 *
 * `catalog.json` is fetched with `no-cache` so it is always current, but the
 * page and the module are ordinary requests under GitHub Pages' `max-age=600`.
 * A deploy that bumps the schema therefore hands a returning visitor a fresh
 * catalogue and a ten-minute-stale module, which fails hard: the Store refuses
 * a schema it was not built for, and the page shows nothing at all. Deploying
 * both together does not help, because the browser does not fetch them together.
 *
 * Putting the schema in the query means a bump is a different URL and cannot be
 * served from a cache filled before it. Keyed on the schema rather than the
 * build timestamp on purpose: only a schema change can break this pairing, and
 * keying on the timestamp would re-download 175 kB after every catalogue build
 * to fix a problem those builds do not cause.
 *
 * The `.wasm` needs the query too. The glue resolves it relative to its own URL,
 * which drops the query, so it is passed explicitly.
 */
async function loadModule(schema) {
  const key = `v${schema}`;
  const module = await import(`./lib/kira_wasm.js?${key}`);
  const binary = new URL('./lib/kira_wasm_bg.wasm', import.meta.url);
  await module.default({ module_or_path: `${binary}?${key}` });

  ({
    Store,
    crc_is_valid: crcIsValid,
    payload_bounds: payloadBounds,
    read_header: readHeader,
    read_variant: readVariant,
    source_ref: sourceRef,
  } = module);
}

const CAN_WRITE = typeof window.showDirectoryPicker === 'function';
const DATA_BASE = new URL('data', location.href).href.replace(/\/$/, '');
/** Bytes of the fixed header, all a scan needs to read per app. */
const HEADER_LEN = 48;

/** Not an app: the SDK's own docs note SharedData lives alongside app folders. */
const NON_APP_DIRS = new Set(['sharedata', 'shareddata', 'system', '.trashes', '.spotlight-v100']);

const el = (id) => document.getElementById(id);

/**
 * Which installer this visitor most likely wants.
 *
 * The script tier is exactly the browsers that cannot write to the drive, i.e.
 * Firefox and Safari, and neither implements `userAgentData` -- so the deprecated
 * `navigator.platform` is the signal that actually answers here, with the UA
 * string behind it. Anything unrecognised gets the shell script, which is the
 * safer wrong guess: it refuses to run rather than touching a drive.
 */
function detectScriptKind() {
  const platform = navigator.userAgentData?.platform || navigator.platform || '';
  if (/^win/i.test(platform)) return 'ps1';
  if (platform) return 'sh';
  return /windows/i.test(navigator.userAgent) ? 'ps1' : 'sh';
}

/**
 * How to eject, in the words of whatever they are running.
 *
 * Same signal as detectScriptKind(): `navigator.platform` is deprecated but is
 * the one that actually answers in the browsers this has to serve.
 */
function ejectHint() {
  const platform = navigator.userAgentData?.platform || navigator.platform || '';
  if (/^win/i.test(platform)) return 'right-click the drive → Eject, or Safely Remove Hardware';
  if (/mac/i.test(platform)) return 'Finder → the eject arrow, or `diskutil eject "UNA WATCH"`';
  if (/linux/i.test(platform)) return '`udisksctl unmount -b /dev/…`, or eject it in your file manager';
  return "your file manager's eject or unmount — not just unplugging it";
}

const state = {
  /** Rust-side catalogue and version pins. */
  store: null,
  /** 'write' | 'read' | null */
  mode: null,
  /** FileSystemDirectoryHandle, write mode only. */
  appsDir: null,
  /** Plain objects matching the Rust `Installed` shape. */
  installed: [],
  /** Most recent plan from the store. */
  plan: null,
  /** Blobs kept from a read-mode scan so Verify can hash without re-picking. */
  blobs: new Map(),
  /**
   * AppIDs the user has unticked in the pending list.
   *
   * Held as exclusions rather than selections so that everything offered is
   * ticked by default, and so an app that appears after a re-scan arrives
   * selected instead of silently sitting out the install.
   */
  excluded: new Set(),
  /**
   * What has been typed into each app's settings form, keyed by AppID.
   *
   * Held outside the DOM because a card is rebuilt whenever anything else on
   * the page changes — pinning a version, re-scanning the watch — and losing a
   * half-typed id to an unrelated re-render would be maddening.
   */
  configDraft: new Map(),
  /**
   * Explicit open/closed choices for settings forms, for the same reason.
   *
   * A Map rather than a Set of the open ones, because absent has to mean "not
   * chosen": a required form defaults to open, so "not in the set" cannot stand
   * in for closed without wiping that default on the next re-render.
   */
  configOpen: new Map(),
  /** Apps whose existing settings file has already been read off the watch. */
  configLoaded: new Set(),
};

// ---------------------------------------------------------------- utilities

function log(message, cls = '') {
  const box = el('status');
  box.hidden = false;
  const line = document.createElement('div');
  if (cls) line.className = cls;
  line.textContent = message;
  box.appendChild(line);
  box.scrollTop = box.scrollHeight;
}

function clearLog() {
  const box = el('status');
  box.textContent = '';
  box.hidden = true;
}

async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

function fmtSize(bytes) {
  return bytes >= 1024 * 1024
    ? `${(bytes / 1024 / 1024).toFixed(2)} MB`
    : `${Math.round(bytes / 1024)} kB`;
}

/**
 * Hash the code within a .uapp: everything between the header and the CRC.
 *
 * The whole 48-byte header is excluded, not only the version stamp — so the
 * AppID, LibC ABI, flags, display name and icon lengths do not reach it either.
 */
async function payloadHash(bytes) {
  const { start, end } = payloadBounds(bytes.length);
  return sha256Hex(bytes.subarray(start, end));
}

// ------------------------------------------------- persisted directory handle

const IDB_NAME = 'kira';
const IDB_STORE = 'handles';

function idb() {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(IDB_NAME, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(IDB_STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/** Run a write and wait for the transaction, discarding any result. */
async function idbWrite(work) {
  const db = await idb();
  try {
    await new Promise((resolve, reject) => {
      const tx = db.transaction(IDB_STORE, 'readwrite');
      work(tx.objectStore(IDB_STORE));
      tx.oncomplete = resolve;
      tx.onerror = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

const idbSet = (key, value) => idbWrite((store) => store.put(value, key));
const idbDelete = (key) => idbWrite((store) => store.delete(key));

/**
 * Read one key.
 *
 * Resolves the request's own `result`, which is `undefined` when the key is
 * absent — worth being explicit about, since resolving the IDBRequest instead
 * yields a truthy object that looks like a stored value.
 */
async function idbGet(key) {
  const db = await idb();
  try {
    return await new Promise((resolve, reject) => {
      const request = db.transaction(IDB_STORE, 'readonly').objectStore(IDB_STORE).get(key);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}

// ------------------------------------------------------------------ catalogue

/**
 * Fetch the published catalogue, always current.
 *
 * Parsing is left to the browser's own JSON parser and handed over as an object:
 * linking a second JSON parser into the WebAssembly module cost about 16 kB
 * gzipped for no benefit. It is read before the module is loaded, because its
 * schema is what decides which module to ask for.
 */
async function fetchCatalog() {
  const res = await fetch(`${DATA_BASE}/catalog.json`, { cache: 'no-cache' });
  if (!res.ok) throw new Error(`catalog.json: HTTP ${res.status}`);
  return res.json();
}

function startCatalog(catalog) {
  // The store validates the schema and throws if it is not the expected one.
  state.store = new Store(catalog);

  const when = new Date(state.store.generated).toLocaleDateString();
  el('catalogue-meta').textContent =
    `${state.store.appCount} apps · ${state.store.versionCount} versions · ` +
    `${state.store.releaseCount} releases · built ${when}`;
  renderReleaseNotes();
}

/** A list of parsed change lines, each linked to its pull request. */
function renderChanges(changes) {
  const list = document.createElement('ul');
  list.className = 'changes';
  for (const change of changes) {
    const item = document.createElement('li');

    if (change.breaking) {
      const flag = document.createElement('span');
      flag.className = 'breaking';
      flag.textContent = 'breaking';
      item.appendChild(flag);
    }
    if (change.scopes.length > 0) {
      const scope = document.createElement('span');
      scope.className = 'scope';
      scope.textContent = change.scopes.join(', ');
      item.appendChild(scope);
    }

    // textContent, never innerHTML: this is prose from another project.
    item.appendChild(document.createTextNode(change.subject));

    if (change.url) {
      const link = document.createElement('a');
      link.className = 'pr';
      link.href = change.url;
      link.rel = 'noopener noreferrer';
      link.target = '_blank';
      link.textContent = change.pr ? `#${change.pr}` : 'PR';
      if (change.author) link.title = `by @${change.author}`;
      item.appendChild(link);
    }
    list.appendChild(item);
  }
  return list;
}

/** What a release did to the apps, in words, from the binary comparison. */
function describeEffect(effect) {
  const parts = [];
  if (effect.changed.length > 0) {
    // Naming every app is unreadable when a library change relinks all of them.
    const shown = effect.changed.slice(0, 4).join(', ');
    const rest = effect.changed.length - 4;
    parts.push(
      `code changed in ${effect.changed.length} app${effect.changed.length === 1 ? '' : 's'}: ` +
        (rest > 0 ? `${shown} and ${rest} more` : shown),
    );
  }
  if (effect.unchanged > 0) parts.push(`${effect.unchanged} unchanged`);
  if (effect.unknown > 0) parts.push(`${effect.unknown} not comparable`);
  if (effect.firstSeen > 0) parts.push(`${effect.firstSeen} first published here`);
  return parts.length > 0 ? parts.join(' · ') : 'no apps in this release';
}

/**
 * Upstream release bodies, sorted by what they can actually affect.
 *
 * Every release stamps every app, and most of what a release body describes is
 * documentation, the simulator or build tooling — none of which reaches a watch.
 * So each release leads with what its binaries did, then the changes that could
 * have caused that, with the repository churn collapsed beneath. Nothing is
 * dropped: the original body stays available verbatim.
 *
 * Rendered as text throughout, never as HTML or parsed Markdown: it is
 * third-party content from another project's releases.
 */
/**
 * How many releases stay in view before the rest are folded away.
 *
 * Three: the newest, which is the one anybody is deciding about, plus enough to
 * see whether it is unusual. Past that it is history, and thirteen equal-weight
 * rows made this the tallest thing on the page.
 */
const RELEASES_UPFRONT = 3;

/**
 * The one-line version of what a release did, for a row that is closed.
 *
 * `describeEffect` names the apps, which wraps a closed row onto two lines and
 * then repeats itself down the page — seven consecutive releases here changed
 * the very same six apps. Closed, the count is enough; the names are one click
 * away.
 */
function summariseEffect(effect) {
  const plural = (n) => (n === 1 ? '' : 's');
  if (effect.changed.length > 0) {
    return `${effect.changed.length} app${plural(effect.changed.length)} changed`;
  }
  if (effect.firstSeen > 0) {
    return `${effect.firstSeen} app${plural(effect.firstSeen)} first published`;
  }
  if (effect.unchanged + effect.unknown > 0) return 'no app code changed';
  return 'no apps in this release';
}

function renderRelease(release, open) {
  const box = document.createElement('details');
  box.className = 'release';
  box.open = open;

  const summary = document.createElement('summary');
  const tag = document.createElement('strong');
  tag.textContent = release.tag;
  summary.appendChild(tag);
  const when = document.createElement('span');
  when.className = 'muted';
  const date = release.publishedAt
    ? new Date(release.publishedAt).toLocaleDateString(undefined, {
        year: 'numeric',
        month: 'short',
        day: 'numeric',
      })
    : 'date unknown';
  when.textContent =
    ` ${date}${release.isPrerelease ? ' · pre-release' : ''} · ${summariseEffect(release.effect)}`;
  summary.appendChild(when);
  box.appendChild(summary);

  // The full account, naming the apps, once the row is open — it is what the
  // summary just abbreviated, so it comes first.
  const effect = document.createElement('p');
  effect.className = 'muted release-effect';
  effect.textContent = describeEffect(release.effect);
  box.appendChild(effect);

  const { changes } = release;
  if (changes.shipped.length > 0) {
    box.appendChild(heading('Changes that reach the watch'));
    box.appendChild(renderChanges(changes.shipped));
  }

  if (changes.other.length > 0) {
    const rest = document.createElement('details');
    rest.className = 'other';
    const restSummary = document.createElement('summary');
    restSummary.textContent = `Docs, simulator and tooling (${changes.other.length})`;
    rest.appendChild(restSummary);
    rest.appendChild(renderChanges(changes.other));
    box.appendChild(rest);
  }

  if (changes.prose.length > 0) {
    const extra = document.createElement('p');
    extra.className = 'muted';
    extra.textContent = changes.prose.join(' ');
    box.appendChild(extra);
  }

  if (changes.shipped.length === 0 && changes.other.length === 0) {
    const none = document.createElement('p');
    none.className = 'muted';
    none.textContent = 'No release notes published upstream.';
    box.appendChild(none);
  }

  const footer = document.createElement('div');
  footer.className = 'release-foot';
  if (release.notes) {
    const verbatim = document.createElement('details');
    const vs = document.createElement('summary');
    vs.textContent = 'Upstream notes, verbatim';
    verbatim.appendChild(vs);
    const body = document.createElement('pre');
    body.className = 'notes';
    body.textContent = release.notes;
    verbatim.appendChild(body);
    footer.appendChild(verbatim);
  }
  if (release.url) {
    const link = document.createElement('a');
    link.className = 'dl';
    link.href = release.url;
    link.rel = 'noopener noreferrer';
    link.target = '_blank';
    link.textContent = 'Upstream release →';
    footer.appendChild(link);
  }
  box.appendChild(footer);

  return box;
}

function renderReleaseNotes() {
  const root = el('release-notes');
  root.textContent = '';

  const releases = state.store.releases();
  const upfront = releases.slice(0, RELEASES_UPFRONT);
  const older = releases.slice(RELEASES_UPFRONT);

  // Only the newest starts open; the two behind it are context, not reading.
  upfront.forEach((release, index) => root.appendChild(renderRelease(release, index === 0)));

  if (older.length > 0) {
    const box = document.createElement('details');
    box.className = 'older-releases';
    const summary = document.createElement('summary');
    summary.textContent = `${older.length} older release${older.length === 1 ? '' : 's'}`;
    box.appendChild(summary);
    for (const release of older) box.appendChild(renderRelease(release, false));
    root.appendChild(box);
  }
}

function heading(text) {
  const head = document.createElement('h4');
  head.className = 'changes-head';
  head.textContent = text;
  return head;
}

function statusLabel(entry) {
  switch (entry.status) {
    case 'install':
      return ['Not installed', 'install'];
    case 'update':
      // A version-only bump is not a neutral "update" — saying so would imply
      // new code that is not there. Deliberately not styled as attention-worthy.
      return entry.identicalPayload
        ? [`${entry.installed.version} → ${versionLabel(entry.app)} · same code`, '']
        : [`Update ${entry.installed.version} → ${versionLabel(entry.app)}`, 'update'];
    case 'current':
      // Which build it is matters: the vendor's binary for the same version is
      // equivalent, not stale.
      return [
        entry.recognised === 'upstream-build' ? 'Up to date · vendor build' : 'Up to date',
        'current',
      ];
    case 'newer-on-watch':
      return [`Watch has ${entry.installed.version}`, ''];
    case 'different-build':
      // Possibly the user's own build. Report it rather than offering to replace it.
      return [`${entry.installed.version} · unrecognised build`, ''];
    case 'corrupt':
      // A file failing its CRC is silently ignored by the watch, so the app never
      // appears. Always worth replacing.
      return ['Corrupt — reinstall', 'update'];
    case 'superseded':
      // Another app owns this on-device folder, so installing this one could
      // leave the watch booting whichever .uapp it found first.
      return ['Superseded — not installable', ''];
    case 'retired':
      // Withdrawn by whoever publishes it. The reason is on the card; here it
      // matters most that a watch already carrying it is named, not nagged.
      return [entry.installed ? 'Withdrawn — installed' : 'Withdrawn', ''];
    case 'folder-taken':
      // Something Kira does not recognise is already in the folder this would be
      // written to, and installing clears other .uapp files out of it. Naming the
      // occupant is the only part of this a user can act on.
      return [
        entry.blocking ? `Folder used by ${entry.blocking.name}` : 'Folder used by another app',
        '',
      ];
    default:
      return [entry.status, ''];
  }
}

/**
 * The four app types are genuinely different things, so the catalogue is
 * sectioned rather than one flat grid. Order is most-to-least substantial;
 * empty sections are omitted.
 */
const TYPE_SECTIONS = [
  {
    type: 'Activity',
    heading: 'Activities',
    blurb: 'Record a session — sensors, laps, and a saved activity file when you finish.',
  },
  { type: 'Utility', heading: 'Utilities', blurb: 'Standalone apps you open from the launcher.' },
  {
    type: 'Glance',
    heading: 'Glances',
    blurb: 'Compact widgets for the 240×60 notification area, not full-screen apps.',
  },
  { type: 'Clockface', heading: 'Clockfaces', blurb: 'Watch faces.' },
];

/**
 * Where the binary on offer came from.
 *
 * The recipe hangs off the title, since that plus the published inputs is what
 * makes a Kira build reproducible rather than merely asserted. A submitted app
 * names its repository and the commit that was compiled; the commit is not a
 * link, because the repository URL is whatever the manifest gave and building a
 * commit URL from it would be a guess about the host.
 *
 * This line is the whole difference between a submitted app and one of UNA's —
 * there is no badge and no separate section, because where a binary came from is
 * information, not a ranking.
 */
function renderProvenance(app, selected) {
  const provenance = document.createElement('div');
  provenance.className = 'meta provenance';
  const built = selected.builtFrom;

  if (selected.origin !== 'kira') {
    provenance.textContent = "the vendor's own build";
    return provenance;
  }

  provenance.append('built by Kira from ');
  // Whether this is the vendor's binary or merely built from the vendor's
  // source. The catalogue has recorded it all along and the card never showed
  // it, which left every SDK app reading as though Kira and UNA ship the same
  // bytes. They do not, and until the SDK carries the path-independence fix they
  // will not — appended below, after the source is named.
  if (app.publisher) {
    const repo = document.createElement('a');
    // Manifests are re-validated at every catalogue build, so this is https.
    repo.href = app.publisher.repo;
    repo.rel = 'noopener noreferrer';
    repo.target = '_blank';
    repo.textContent = app.publisher.repo.replace(/^https:\/\//, '').replace(/\.git$/, '');
    provenance.appendChild(repo);

    const source = built ? sourceRef(built.appSource) : undefined;
    if (source) {
      const rev = document.createElement('span');
      rev.className = 'rev';
      rev.textContent = ` at ${source.rev.slice(0, 12)}`;
      rev.title = source.rev;
      provenance.appendChild(rev);
    }
  } else {
    provenance.append('source');
  }

  // Only meaningful where the vendor published a binary to compare against; a
  // submission has none, so `null` stays silent rather than being reported as a
  // difference nobody measured.
  if (selected.matchesUpstream === false) {
    const differs = document.createElement('span');
    differs.className = 'differs';
    differs.textContent = ' · not the vendor’s bytes';
    differs.title =
      `This is Kira's build of the same source, not the binary UNA ships.\n` +
      `UNA's build of ${versionLabel(selected)} hashes ${(selected.upstreamSha256 ?? '').slice(0, 16)}…\n` +
      'The two are expected to converge once the SDK carries its path-independence fix.';
    provenance.appendChild(differs);
  } else if (selected.matchesUpstream === true) {
    const same = document.createElement('span');
    same.textContent = ' · identical to the vendor’s build';
    provenance.appendChild(same);
  }

  if (built) {
    provenance.title =
      `recipe ${built.recipe}\nsource ${built.appSource}\ntoolchain ${built.toolchain}` +
      (app.publisher ? `\nmaintained by @${app.publisher.maintainer}` : '');
  }
  return provenance;
}

/**
 * How a build is named, and how a selection refers to it: `1.4.0` or `1.4.0-rc1`.
 *
 * Mirrors `prerelease::label` in kira-core, which is the source of truth. The
 * version alone stopped identifying a build once release candidates were
 * published: `apps-v1.4.0-rc1` and `apps-v1.4.0` both stamp 1.4.0, so two entries
 * would read the same and this picker could not tell a reader them apart.
 */
function versionLabel(version) {
  return version.prerelease ? `${version.version}-${version.prerelease}` : version.version;
}

function renderCard(app, entry) {
  const selected = app.versions.find((v) => versionLabel(v) === app.selected) ?? app.versions[0];

  const card = document.createElement('div');
  card.className = 'card';
  card.id = cardAnchor(app.appId);

  if (app.icon) {
    const img = document.createElement('img');
    img.src = `${DATA_BASE}/${app.icon}`;
    img.alt = '';
    img.width = 48;
    img.height = 48;
    card.appendChild(img);
  } else {
    // Glance apps are commonly built without icons, so this is the norm rather
    // than a failure — show a lettered tile instead of an empty gap.
    const placeholder = document.createElement('div');
    placeholder.className = 'noicon';
    placeholder.setAttribute('aria-hidden', 'true');
    placeholder.textContent = (app.name.match(/[A-Za-z0-9]/)?.[0] ?? '?').toUpperCase();
    card.appendChild(placeholder);
  }

  const body = document.createElement('div');
  body.className = 'body';

  const title = document.createElement('h3');
  title.textContent = app.name;
  body.appendChild(title);

  const meta = document.createElement('div');
  meta.className = 'meta';
  meta.textContent =
    `${fmtSize(selected.size)}${selected.autostart ? ' · autostarts' : ''} · ${app.history}`;
  body.appendChild(meta);

  // A variant alias: a code-less .uapp that runs another app's binary under its
  // own launcher entry. The whole sentence comes from kira-core, including which
  // app it names and whether that app is even in this catalogue — the descriptor
  // in the binary is what says so, and working it out here would be the second
  // copy of a rule. textContent: the target's display name comes out of a header.
  if (app.variant) {
    const note = document.createElement('div');
    note.className = 'meta variant';
    note.textContent = app.variant;
    note.title =
      'A variant is packed from a manifest rather than compiled: it carries icons, ' +
      'a target AppID and a small JSON config, and no code at all. Without the app ' +
      'it names there is nothing for it to run.';
    body.appendChild(note);
  }

  body.appendChild(renderProvenance(app, selected));

  // What the publisher says this version changed. Deliberately below the
  // byte-derived history line rather than instead of it: one is an assertion,
  // the other is evidence, and the card keeps them apart. textContent, never
  // innerHTML — this is prose from another project.
  if (selected.notes) {
    const note = document.createElement('p');
    note.className = 'meta version-notes';
    note.textContent = selected.notes;
    body.appendChild(note);
  }

  // Withdrawn by whoever publishes it. The reason is shown rather than hidden
  // behind a flag: it is the only part of a withdrawal that helps anyone whose
  // watch is already carrying the app.
  const withdrawn = app.retired ?? selected.retired;
  if (withdrawn) {
    const note = document.createElement('div');
    note.className = 'meta superseded';
    note.textContent = `withdrawn — ${withdrawn}`;
    note.title = app.retired
      ? 'This app is no longer offered for installation.'
      : `Version ${versionLabel(selected)} is no longer offered for installation.`;
    body.appendChild(note);
  }

  // A candidate, published so an app that ships in one is reachable before the
  // release lands — Stopwatch and Timer first appeared in apps-v1.4.0-rc1. Stated
  // on the card and not only in the version picker, which is absent entirely when
  // the candidate is the only build there is.
  if (selected.prerelease) {
    const note = document.createElement('div');
    note.className = 'meta prerelease';
    // "may not start" is on the visible line, not left to the tooltip, because of
    // how it fails. An app refuses to launch when the kernel is older than the
    // interface version it was compiled against, and the interface went 2 to 3
    // between apps-v1.3.0 and apps-v1.4.0 — so a 1.4 candidate on a watch still on
    // 2 does nothing, and what the owner sees is an app that will not open and
    // cannot be exited. That reads as a broken watch rather than a wrong choice,
    // which is the sort of thing somebody deserves to be told before installing
    // rather than after.
    //
    // "may", because Kira genuinely cannot tell. The requirement is compiled into
    // the app and is not in the .uapp header, and the watch only reports its
    // firmware over BLE, which this page has no access to. See
    // UNAWatch/una-sdk#262.
    note.textContent = `pre-release · ${selected.tag} · may not start on an older watch`;
    // The only place a bare version is deliberate: both of these name the release
    // this candidate is *for*, which is the thing that does not exist yet.
    // versionLabel here would say "replaced by 1.4.0-rc1 proper".
    note.title =
      `UNA published this as a release candidate, not a full release. It is their ` +
      `own build of ${selected.version}, offered here so you do not have to wait ` +
      `for the final or build it yourself. Expect it to be replaced by ${selected.version} proper.\n\n` +
      `A candidate is built against a newer SDK than the last full release, and an ` +
      `app will not launch on a watch whose kernel is older than the SDK it was ` +
      `built against — it stops before it draws anything. Nothing in a .uapp says ` +
      `which kernel it needs and the watch only reports its firmware over ` +
      `Bluetooth, so Kira cannot check this for you. If the app does nothing after ` +
      `installing and rebooting, that is the likely reason.`;
    body.appendChild(note);
  }

  // Upstream reassigned AppIDs: three Glances carry one id up to apps-v0.1.9-rc1
  // and a different one after. Those are separate identities to the watch and the
  // phone, so they stay separate entries — but say which is which.
  if (app.ambiguousName || app.supersededBy) {
    const id = document.createElement('div');
    id.className = 'meta appid';
    id.textContent = `AppID ${app.appId}`;
    id.title = app.supersededBy
      ? `Replaced by AppID ${app.supersededBy}`
      : 'Another entry shares this name under a different AppID';
    body.appendChild(id);
  }

  // An app whose folder belongs to another cannot be installed beside it. Kept
  // listed because its binaries are still downloadable.
  if (app.supersededBy) {
    const note = document.createElement('div');
    note.className = 'meta superseded';
    note.textContent = 'download only';
    note.title =
      `${app.folder} on the watch belongs to AppID ${app.supersededBy}, ` +
      'which has newer versions. Installing both could leave the watch running the wrong one.';
    body.appendChild(note);
  }

  // Upstream offers no way to fetch a specific build, so every published version
  // is selectable here. Newest is the default.
  if (app.versions.length > 1) {
    const picker = document.createElement('select');
    picker.className = 'version';
    picker.setAttribute('aria-label', `Version of ${app.name}`);
    for (const version of app.versions) {
      const option = document.createElement('option');
      const label = versionLabel(version);
      option.value = label;
      const tags = [];
      // From the store, not from the head of the list: a candidate outranks an
      // earlier release but is not what gets offered. See `App::latest`.
      if (label === app.latestLabel) tags.push('latest');
      // Upstream's own candidate, published so an app that ships in one is
      // reachable before the release lands. Said on every entry, not just the
      // newest, because the number alone does not show it.
      if (version.prerelease) tags.push('pre-release');
      if (version.changed === false) tags.push('same code');
      // Still selectable and still downloadable — a watch carrying it has to be
      // able to find out what it has — but never offered for installation.
      if (version.retired) tags.push('withdrawn');
      option.textContent = tags.length ? `${label} · ${tags.join(' · ')}` : label;
      option.selected = label === app.selected;
      picker.appendChild(option);
    }
    picker.addEventListener('change', () => void pinVersion(app.appId, picker.value));
    body.appendChild(picker);
  }

  if (entry) {
    const [text, cls] = statusLabel(entry);
    const badge = document.createElement('span');
    badge.className = `badge ${cls}`;
    badge.textContent = text;
    body.appendChild(badge);
  }

  // Offered whether or not a watch is connected: connecting used to replace this
  // with a status badge, which left a plan row pointing at a card with nothing to
  // act on.
  const dl = document.createElement('a');
  dl.className = 'dl';
  dl.href = `${DATA_BASE}/${selected.download}`;
  // The label, not the bare version: a candidate's button must not read as
  // though it offers the release.
  dl.textContent = `Download ${versionLabel(selected)}`;
  dl.setAttribute('download', selected.file);
  dl.title = `${selected.file} → Apps/${app.folder}/`;
  body.appendChild(dl);

  // Only a submission can declare one, and most do not. A value the app cannot
  // work without goes *above* the download: it is a precondition rather than an
  // afterthought, and underneath the button is where it gets missed.
  if (app.config) {
    const setup = renderConfig(app);
    if (configIsRequired(app)) body.insertBefore(setup, dl);
    else body.appendChild(setup);
  }

  card.appendChild(body);
  return card;
}

// ------------------------------------------------------------------ settings

/**
 * Read the value a dotted path points at, or '' if the file does not have it.
 *
 * The file is somebody's hand-edited JSON as often as it is Kira's, so every
 * step has to survive the wrong shape.
 */
function atPath(doc, path) {
  let node = doc;
  for (const key of path.split('.')) {
    if (node === null || typeof node !== 'object' || Array.isArray(node)) return '';
    node = node[key];
  }
  return typeof node === 'string' ? node : '';
}

/**
 * Whether this app declares a value it cannot work without.
 *
 * The submitter's word, and unverifiable against the binary, so it only ever
 * changes how the page presents the form — never whether an install is allowed.
 */
function configIsRequired(app) {
  return Boolean(app.config?.fields?.some((field) => field.required));
}

/**
 * The required fields the watch has no value for, given what is on it.
 *
 * @param doc Parsed settings file, or null when there is no readable one — which
 *            is itself the answer: every required field is missing.
 */
function missingRequired(spec, doc) {
  return spec.fields.filter((field) => field.required && !(doc && atPath(doc, field.path)));
}

/** This app's row in the current plan, if a watch is connected. */
function planEntry(appId) {
  return (state.plan?.entries ?? []).find((entry) => entry.app.appId === appId);
}

/**
 * What owns `Apps/<Folder>/` instead of this app, or null when the folder is the
 * app's own.
 *
 * The planner already works this out for installing, and refuses: it clears other
 * `.uapp` files out of the folder it writes to, so going ahead would delete
 * somebody's app. Writing a settings file is a smaller act with the same mistake
 * underneath -- the directory belongs to another app, and the card has already
 * said so. Nothing about a settings file makes that ownership different, and
 * `Apps/<Folder>/<file>` is exactly where the other app's own config would live.
 *
 * Read from the live plan rather than the entry captured at render, so connecting
 * or ejecting a watch cannot leave a stale answer behind a button.
 *
 * Withdrawal is deliberately not here. A retired app is not offered for
 * installation, but the folder is still its own and a watch may be carrying it --
 * whoever has it should be able to fix its settings.
 */
function configFolderOwner(app) {
  if (app.supersededBy) {
    return `AppID ${app.supersededBy}, which the catalogue lists in that folder with newer versions`;
  }
  const blocking = planEntry(app.appId)?.blocking;
  if (blocking) {
    return `${blocking.name} ${blocking.version}, which is in that folder on the watch now`;
  }
  return null;
}

/** What is already in the app's settings file, or null if there is nothing usable. */
async function readConfig(app) {
  if (!state.appsDir) return null;
  try {
    const dir = await state.appsDir.getDirectoryHandle(app.folder);
    const handle = await dir.getFileHandle(app.config.file);
    const text = await (await handle.getFile()).text();
    // JSON.parse, never eval — and the values only ever reach an input's
    // .value, so a hostile file can misinform but cannot execute.
    const doc = JSON.parse(text);
    return doc && typeof doc === 'object' ? doc : null;
  } catch {
    // Absent, unreadable or not JSON. All three mean "nothing to prefill", and
    // the app itself is what tells its owner which.
    return null;
  }
}

/** Write the assembled document, then read the length back. */
async function writeConfig(app, text) {
  // Checked here and not only where the button is disabled: this is the call that
  // reaches a device, and the answer can change between rendering a card and
  // pressing what is on it.
  const foreign = configFolderOwner(app);
  if (foreign) {
    throw new Error(`Apps/${app.folder}/ belongs to ${foreign}`);
  }
  const bytes = new TextEncoder().encode(text);
  const dir = await state.appsDir.getDirectoryHandle(app.folder, { create: true });
  const handle = await dir.getFileHandle(app.config.file, { create: true });
  const writable = await handle.createWritable();
  try {
    await writable.write(bytes);
    await writable.close();
  } catch (err) {
    await writable.abort?.();
    throw err;
  }
  // Same caveat as installing: this reads the OS write cache, so it catches a
  // short write and proves nothing about flash. Unlike a .uapp there is no CRC
  // to fall back on, so the app's own parse is the real check.
  const back = await handle.getFile();
  if (back.size !== bytes.length) {
    throw new Error(`short write (${back.size}/${bytes.length} bytes)`);
  }
}

/**
 * The per-app settings form.
 *
 * Chromium desktop only, like installing and for the same reason: writing to a
 * removable drive needs the File System Access API. The generated scripts
 * deliberately carry no settings — see the note in the summary.
 */
function renderConfig(app) {
  const spec = app.config;
  const draft = state.configDraft.get(app.appId) ?? {};
  state.configDraft.set(app.appId, draft);

  const required = configIsRequired(app);

  const box = document.createElement('details');
  box.className = required ? 'config config-required' : 'config';
  // Collapsed hides the one thing the app cannot start without, and "Settings"
  // reads as a preference. A required field opens by default; an explicit toggle
  // still wins, so closing it stays closed across re-renders.
  box.open = state.configOpen.get(app.appId) ?? required;
  box.addEventListener('toggle', () => {
    state.configOpen.set(app.appId, box.open);
  });

  const summary = document.createElement('summary');
  summary.textContent = required ? 'Setup' : 'Settings';
  box.appendChild(summary);

  // A folder another app owns is not written to at all -- not the settings file,
  // and not the read that prefills it either.
  const foreign = configFolderOwner(app);
  const writable = state.mode === 'write' && state.appsDir && !foreign;

  const where = document.createElement('p');
  where.className = 'meta';
  // Not "written to" when it will not be: the refusal below would contradict it.
  //
  // "as plain text" is on the visible line rather than only in the tooltip
  // because of what these fields invite: the format exists for a value only its
  // owner knows, and the examples that reach for it are an athlete id, a transit
  // pass, an account token. The first two are nobody's secret. The third is, and
  // whoever is about to type one should not have to hover to find out where it
  // lands.
  where.textContent = foreign
    ? `This app reads Apps/${app.folder}/${spec.file} on the watch.`
    : `Written to Apps/${app.folder}/${spec.file} on the watch, as plain text.`;
  where.title =
    'The watch presents its storage as a USB drive, so this file can be read by ' +
    'anything on any computer it is plugged into, and by any other app on the ' +
    'watch. Fine for an id or a preference. Not somewhere to put a password or an ' +
    'account token you would mind somebody else having.';
  box.appendChild(where);

  if (foreign) {
    const note = document.createElement('p');
    note.className = 'meta config-note bad';
    note.textContent =
      `That folder belongs to ${foreign}, so Kira will not write to it — the ` +
      'settings would land in another app\'s folder.';
    box.appendChild(note);
  } else if (!writable) {
    const note = document.createElement('p');
    note.className = 'meta config-note';
    note.textContent = CAN_WRITE
      ? 'Connect a watch to fill this in.'
      : 'This browser can read the watch but not write to it, so settings have to be ' +
        `typed into Apps/${app.folder}/${spec.file} by hand. The generated install ` +
        'script does not carry them.';
    box.appendChild(note);
  }

  const status = document.createElement('p');
  status.className = 'meta config-status';

  const inputs = new Map();
  for (const field of spec.fields) {
    const label = document.createElement('label');
    label.className = 'config-field';

    const name = document.createElement('span');
    name.textContent = field.title;
    label.appendChild(name);

    const input = document.createElement('input');
    input.type = 'text';
    input.maxLength = field.maxLength;
    input.spellcheck = false;
    input.autocapitalize = 'off';
    input.autocomplete = 'off';
    input.disabled = !writable;
    input.value = draft[field.path] ?? '';
    input.addEventListener('input', () => {
      draft[field.path] = input.value;
      const problem = input.value === '' ? null : state.store.configCheck(app.appId, field.path, input.value);
      input.setCustomValidity(problem ?? '');
      status.textContent = problem ?? '';
      status.className = problem ? 'meta config-status bad' : 'meta config-status';
    });
    label.appendChild(input);
    inputs.set(field.path, input);

    if (field.help) {
      const help = document.createElement('span');
      help.className = 'config-help';
      help.textContent = field.help;
      label.appendChild(help);
    }
    box.appendChild(label);
  }

  // Prefill from the watch once, and only for fields the user has not started
  // typing into — a re-render must not overwrite what is being entered.
  if (writable && !state.configLoaded.has(app.appId)) {
    state.configLoaded.add(app.appId);
    void readConfig(app).then((doc) => {
      for (const field of spec.fields) {
        if (draft[field.path] !== undefined) continue;
        const value = doc ? atPath(doc, field.path) : '';
        if (!value) continue;
        draft[field.path] = value;
        const input = inputs.get(field.path);
        if (input && input.value === '') input.value = value;
      }

      // Only claimed once the watch has actually been read: an install that
      // succeeded and a value that was never written look identical otherwise,
      // which is how somebody ends up with an app that does nothing and no idea
      // why. Says nothing when there is nothing to chase.
      const missing = missingRequired(spec, doc);
      if (missing.length > 0 && !status.textContent) {
        const names = missing.map((f) => f.title).join(', ');
        status.textContent =
          missing.length === 1
            ? `${names} is not set on the watch yet, and the app needs it.`
            : `Not set on the watch yet, and the app needs them: ${names}.`;
        status.className = 'meta config-status warn';
      }
    });
  }

  const save = document.createElement('button');
  save.type = 'button';
  save.className = 'config-save';
  save.textContent = 'Save to watch';
  save.disabled = !writable;
  save.addEventListener('click', () => {
    void (async () => {
      save.disabled = true;
      try {
        const values = {};
        for (const field of spec.fields) values[field.path] = draft[field.path] ?? '';
        // Rust assembles and screens it: the same code the tests cover, and the
        // only place that decides what reaches a device.
        const text = state.store.configDocument(app.appId, values);
        await writeConfig(app, text);
        status.textContent = 'Saved. Eject the watch and reboot it to pick this up.';
        status.className = 'meta config-status ok';
        log(`${app.name} settings → Apps/${app.folder}/${spec.file}`, 'ok');
      } catch (err) {
        status.textContent = String(err.message ?? err);
        status.className = 'meta config-status bad';
      } finally {
        save.disabled = !writable;
      }
    })();
  });
  box.appendChild(save);
  box.appendChild(status);

  return box;
}

function renderCatalogue() {
  const root = el('catalogue');
  root.removeAttribute('aria-busy');
  root.textContent = '';

  const all = state.store.apps();
  // Apps that cannot be installed are listed separately, whether that is because
  // upstream replaced the identity or because whoever published it withdrew it.
  // Leaving them in the grids invites a misclick on something not on offer.
  const isArchived = (a) => Boolean(a.supersededBy || a.retired);
  const apps = all.filter((a) => !isArchived(a));
  const archived = all.filter(isArchived);
  const byId = new Map((state.plan?.entries ?? []).map((e) => [e.app.appId, e]));
  const seen = new Set();

  const section = (heading, blurb, members) => {
    const group = document.createElement('section');
    group.className = 'type-group';

    const head = document.createElement('div');
    head.className = 'type-head';
    const title = document.createElement('h3');
    title.textContent = heading;
    head.appendChild(title);
    const count = document.createElement('span');
    count.className = 'muted';
    count.textContent = `${members.length}`;
    head.appendChild(count);
    group.appendChild(head);

    if (blurb) {
      const note = document.createElement('p');
      note.className = 'type-blurb';
      note.textContent = blurb;
      group.appendChild(note);
    }

    const grid = document.createElement('div');
    grid.className = 'grid';
    for (const app of members) grid.appendChild(renderCard(app, byId.get(app.appId)));
    group.appendChild(grid);
    root.appendChild(group);
  };

  for (const spec of TYPE_SECTIONS) {
    const members = apps.filter((a) => a.type === spec.type);
    members.forEach((a) => seen.add(a.appId));
    if (members.length > 0) section(spec.heading, spec.blurb, members);
  }

  // Anything with a type this build does not know about still gets shown.
  const rest = apps.filter((a) => !seen.has(a.appId));
  if (rest.length > 0) section('Other', '', rest);

  if (archived.length > 0) renderArchive(root, archived, byId);
}

/**
 * Apps that are no longer on offer, for either of the two reasons there are.
 *
 * Collapsed by default and kept out of the grids above. Still listed, and their
 * binaries still downloadable, so that a watch carrying one can be recognised and
 * its owner told why — which is more use than reporting it as something unknown.
 */
function renderArchive(root, archived, byId) {
  const box = document.createElement('details');
  box.className = 'archive';

  const withdrawn = archived.filter((a) => a.retired).length;
  const replaced = archived.length - withdrawn;
  const parts = [];
  if (withdrawn > 0) parts.push(`${withdrawn} withdrawn`);
  if (replaced > 0) {
    parts.push(`${replaced} replaced identit${replaced === 1 ? 'y' : 'ies'}`);
  }

  const summary = document.createElement('summary');
  summary.textContent = `Archived — ${parts.join(', ')}`;
  box.appendChild(summary);

  const both = withdrawn > 0 && replaced > 0;
  const reasons = [];
  if (withdrawn > 0) {
    reasons.push(
      `${both ? 'Some' : 'These'} were withdrawn by whoever publishes them, with the ` +
        'reason on the card.',
    );
  }
  if (replaced > 0) {
    reasons.push(
      `Upstream reassigned ${both ? 'others' : 'these apps'} to new AppIDs; the current ` +
        'versions are listed above under the same names, and these entries keep the old ' +
        'identity.',
    );
  }

  const note = document.createElement('p');
  note.className = 'type-blurb';
  note.textContent =
    `${reasons.join(' ')} None of them can be installed — the binaries stay here so a ` +
    'watch already carrying one can be identified.';
  box.appendChild(note);

  const grid = document.createElement('div');
  grid.className = 'grid';
  for (const app of archived) grid.appendChild(renderCard(app, byId.get(app.appId)));
  box.appendChild(grid);
  root.appendChild(box);
}

/** Pin an app to a version, or back to newest, and re-plan. */
async function pinVersion(appId, version) {
  try {
    state.store.pin(appId, version);
  } catch (err) {
    log(err.message, 'bad');
    return;
  }
  if (state.mode) await refreshInventory();
  else {
    state.plan = null;
    renderCatalogue();
  }
}

// ------------------------------------------------------- reading a connected watch

/** Accept either the volume root or the Apps folder itself. */
async function resolveAppsDir(root) {
  if (root.name.toLowerCase() === 'apps') return root;
  try {
    return await root.getDirectoryHandle('Apps');
  } catch {
    throw new Error(
      `No "Apps" folder inside "${root.name}". Pick the watch's drive, or its Apps folder.`,
    );
  }
}

/** Turn a header plus file facts into the shape the planner expects. */
function installedFrom(header, folder, file, size, extraUapps, variant) {
  return {
    appId: header.appId,
    folder,
    file,
    name: header.name,
    version: header.version,
    size,
    extraUapps,
    // Present only for a variant alias, which the header flag already gave away.
    variant: variant ?? null,
    // Filled by hashInstalled() once the file has been read.
    payloadSha256: null,
    sha256: null,
    crcValid: null,
  };
}

/**
 * The descriptor of an installed .uapp, when the header says it is an alias.
 *
 * A whole extra read, but only for a file the header has already identified as a
 * few kilobytes of alias rather than a few hundred of app — and it is the only
 * way a variant created on the watch can be reported as anything but a stranger,
 * since it ships in no release and can be in no catalogue.
 */
async function readVariantOf(header, file) {
  if (!header.variantAlias) return undefined;
  try {
    return readVariant(new Uint8Array(await file.arrayBuffer()));
  } catch {
    // Unreadable descriptors are the planner's business, not the scan's; a file
    // that will not parse at all is already reported by the caller.
    return undefined;
  }
}

/** Read installed apps by parsing each folder's .uapp header. */
async function readInstalledFromHandles(appsDir) {
  const installed = [];
  for await (const [name, handle] of appsDir.entries()) {
    if (handle.kind !== 'directory') continue;
    if (NON_APP_DIRS.has(name.toLowerCase())) continue;

    const uapps = [];
    for await (const [fileName, fileHandle] of handle.entries()) {
      if (fileHandle.kind === 'file' && fileName.toLowerCase().endsWith('.uapp')) {
        uapps.push({ fileName, fileHandle });
      }
    }
    if (uapps.length === 0) continue;

    // The watch loads whichever .uapp it finds first, so more than one is a
    // hazard: it may still be booting the old build.
    uapps.sort((a, b) => a.fileName.localeCompare(b.fileName));
    const [{ fileName, fileHandle }] = uapps;
    const file = await fileHandle.getFile();

    try {
      const head = new Uint8Array(await file.slice(0, HEADER_LEN).arrayBuffer());
      const header = readHeader(head, file.size);
      installed.push(
        installedFrom(
          header,
          name,
          fileName,
          file.size,
          uapps.slice(1).map((u) => u.fileName),
          await readVariantOf(header, file),
        ),
      );
    } catch (err) {
      log(`  ${name}/${fileName}: not a readable .uapp (${err.message})`, 'bad');
    }
  }
  return installed;
}

/** Same, from an <input webkitdirectory> FileList. */
async function readInstalledFromFiles(fileList) {
  const files = [...fileList].filter((f) => f.name.toLowerCase().endsWith('.uapp'));
  const segments = (f) => (f.webkitRelativePath || f.name).split('/');

  // Prefer files under an "Apps" segment; if the user picked the Apps folder
  // directly there may not be one, in which case use the parent folder name.
  const underApps = files.filter((f) => segments(f).some((s) => s.toLowerCase() === 'apps'));
  const chosen = underApps.length > 0 ? underApps : files;

  const byFolder = new Map();
  for (const file of chosen) {
    const parts = segments(file);
    const appsAt = parts.findIndex((s) => s.toLowerCase() === 'apps');
    const folder = appsAt >= 0 ? parts[appsAt + 1] : parts[parts.length - 2];
    if (!folder || NON_APP_DIRS.has(folder.toLowerCase())) continue;
    if (!byFolder.has(folder)) byFolder.set(folder, []);
    byFolder.get(folder).push(file);
  }

  const installed = [];
  state.blobs.clear();
  for (const [folder, group] of [...byFolder].sort((a, b) => a[0].localeCompare(b[0]))) {
    group.sort((a, b) => a.name.localeCompare(b.name));
    const [file] = group;
    try {
      const head = new Uint8Array(await file.slice(0, HEADER_LEN).arrayBuffer());
      const header = readHeader(head, file.size);
      installed.push(
        installedFrom(
          header,
          folder,
          file.name,
          file.size,
          group.slice(1).map((f) => f.name),
          await readVariantOf(header, file),
        ),
      );
      // Kept so Verify can hash without asking for the folder again.
      state.blobs.set(`${folder}/${file.name}`, file);
    } catch (err) {
      log(`  ${folder}/${file.name}: not a readable .uapp (${err.message})`, 'bad');
    }
  }
  return installed;
}

/** Read a whole installed .uapp, in either connection mode. */
async function readInstalledBytes(installed) {
  const blob = state.blobs.get(`${installed.folder}/${installed.file}`);
  if (blob) return new Uint8Array(await blob.arrayBuffer());
  const dir = await state.appsDir.getDirectoryHandle(installed.folder);
  const handle = await dir.getFileHandle(installed.file);
  return new Uint8Array(await (await handle.getFile()).arrayBuffer());
}

// ------------------------------------------------------------------- installing

/** Fetch a catalogue app and check it end to end before it touches the watch. */
async function fetchVerified(app) {
  const res = await fetch(`${DATA_BASE}/${app.download}`, { cache: 'no-cache' });
  if (!res.ok) throw new Error(`download failed: HTTP ${res.status}`);
  const bytes = new Uint8Array(await res.arrayBuffer());

  if (bytes.length !== app.size) {
    throw new Error(`size mismatch: expected ${app.size}, got ${bytes.length}`);
  }
  if ((await sha256Hex(bytes)) !== app.sha256) {
    throw new Error('SHA-256 mismatch against the catalogue');
  }
  // A bad CRC would be dropped silently by the kernel — the app would simply
  // never appear in the launcher. Refuse to write one.
  if (!crcIsValid(bytes)) throw new Error('CRC32 footer is invalid');

  return bytes;
}

async function installOne(entry) {
  const { app } = entry;
  log(`${app.name} ${versionLabel(app)} → Apps/${app.folder}/${app.file}`);

  const bytes = await fetchVerified(app);
  const dir = await state.appsDir.getDirectoryHandle(app.folder, { create: true });

  const handle = await dir.getFileHandle(app.file, { create: true });
  const writable = await handle.createWritable();
  try {
    await writable.write(bytes);
    await writable.close();
  } catch (err) {
    await writable.abort?.();
    throw err;
  }

  // Read back the size. This still reads through the OS write cache, so it
  // catches a short write but proves nothing about flash — that is what the
  // eject-then-Verify step is for.
  const back = await handle.getFile();
  if (back.size !== bytes.length) {
    // Only remove the half-written file when there is something else in the
    // folder to fall back to. On a corrupt-reinstall the name being written is
    // the name already there, so deleting it takes the app off the watch
    // entirely — and the old message said "stale binary left in place", which
    // in exactly that case was untrue.
    const others = [];
    for await (const [name, child] of dir.entries()) {
      if (child.kind === 'file' && name.toLowerCase().endsWith('.uapp') && name !== app.file) {
        others.push(name);
      }
    }
    if (others.length > 0) {
      await dir.removeEntry(app.file).catch(() => {});
      throw new Error(
        `short write (${back.size}/${bytes.length}); removed it, ${others.join(', ')} left in place`,
      );
    }
    throw new Error(
      `short write (${back.size}/${bytes.length}); left in place — it is the only ` +
        `.uapp in Apps/${app.folder}/, so removing it would take the app off the watch. ` +
        'Re-install before rebooting.',
    );
  }

  // Only now is it safe to remove older binaries.
  for await (const [name, child] of dir.entries()) {
    if (child.kind !== 'file') continue;
    if (!name.toLowerCase().endsWith('.uapp') || name === app.file) continue;
    await dir.removeEntry(name);
    log(`  removed stale ${name}`);
  }

  log('  ok', 'ok');
}

async function installAll() {
  const jobs = inWriteOrder(selectedJobs());
  if (jobs.length === 0) return;

  clearLog();
  setBusy(true);
  let failed = 0;
  for (const entry of jobs) {
    try {
      await installOne(entry);
    } catch (err) {
      failed++;
      log(`  FAILED: ${err.message}`, 'bad');
    }
  }

  log('');
  log(
    failed === 0
      ? `${jobs.length} app(s) handed to the operating system.`
      : `${failed} of ${jobs.length} failed.`,
    failed === 0 ? 'ok' : 'bad',
  );
  // "Written" here means the bytes reached the operating system, which is all a
  // page can know. On a removable FAT volume they can sit in the write cache
  // until the drive is ejected -- pull the cable instead and the watch still
  // boots the old binary, with nothing anywhere reporting a problem. So the
  // eject is not tidiness after the install; it is the last step *of* it, and
  // saying "written" without saying so is how an install silently does nothing.
  if (failed === 0) {
    log('');
    log('NOT DONE YET — the bytes are in the operating system, not in flash.', 'bad');
    log(`  1. Eject the drive: ${ejectHint()}`, 'bad');
    log('     Not "Forget this watch" here, and not unplugging it — those skip the flush.');
    log('  2. Reconnect it, then press "Verify flash".');
    log('  3. Reboot the watch. The launcher list is only rebuilt at boot.');
  }

  await refreshInventory();
  setBusy(false);
}

// --------------------------------------------------------------------- verify

/**
 * Re-read the device, recovering from a handle that a reconnect invalidated.
 *
 * A reconnected volume is usually a new mount, so the stored handle resolves to
 * nothing — which is exactly the state the page instructs you into, since it
 * tells you to eject and reconnect before verifying. Telling you to go and press
 * another button was better than the raw DOM error it replaced, but it is still
 * two clicks to do the thing you already asked for. Verify is itself a click, so
 * the activation the directory picker needs is normally still alive here.
 *
 * Returns false when the device could not be read and the reason has been logged.
 */
async function readDeviceForVerify() {
  try {
    await refreshInventory();
    return true;
  } catch (err) {
    if (err?.name !== 'NotFoundError') throw err;
  }

  log('The watch was reconnected, so the stored handle no longer resolves.');
  log('Pick the drive again to carry on.');
  try {
    if (!(await connectWithPicker())) {
      log('Cancelled — press "Re-scan watch" when ready, then verify.', 'bad');
      return false;
    }
  } catch (err) {
    log(`Could not re-open it: ${err.message}`, 'bad');
    log('Press "Re-scan watch" to pick it up again, then verify.', 'bad');
    return false;
  }
  // connectWithPicker() re-scans through useRoot(), so the plan is already fresh.
  return true;
}

/**
 * How a verified file reads, and whether it counts against the run.
 *
 * The verdict comes from the planner, which is the only thing that knows both
 * published hashes. Deriving it here from `app.sha256` alone is what reported
 * every vendor-built binary on the watch as a mismatch, on a screen that was
 * simultaneously calling them up to date.
 */
const VERDICTS = {
  match: ['ok', 'ok', false],
  'vendor-match': ['ok · vendor build', 'ok', false],
  'other-version': ['OTHER VERSION', 'bad', true],
  // The version numbers match -- a candidate stamps the version it is a candidate
  // for -- so this says which build rather than which version.
  'candidate-build': ['PRE-RELEASE BUILD — reinstall for the release', 'bad', true],
  unknown: ['UNRECOGNISED', 'bad', true],
  corrupt: ['CORRUPT — the watch is ignoring this', 'bad', true],
  unchecked: ['not read', '', false],
};

/**
 * Check what is actually in flash against the catalogue.
 *
 * Re-reads the device first rather than trusting the inventory in memory: after
 * an eject that inventory describes the previous mount, and reading cold flash
 * is the entire point — before an eject this sees the OS write cache and can
 * report a false OK.
 */
async function verifyFlash() {
  clearLog();
  setBusy(true);
  try {
    // Read mode holds Blobs from a <input webkitdirectory> pick, which cannot be
    // re-read: they are a snapshot of the folder as it was when it was chosen,
    // and after an install script has run they describe the previous state. This
    // used to re-plan against that snapshot and report "All N file(s) verified
    // against flash" having read nothing at all — the exact false OK the two-step
    // verify exists to prevent, in the tier that most needs it.
    if (state.mode !== 'write') {
      log('This browser cannot re-read the watch, so it cannot verify.', 'bad');
      log('Eject, reconnect, and pick the Apps folder again — that scan is the check.', 'bad');
      return;
    }

    log('Verifying — this is only trustworthy if you ejected and reconnected the watch.');
    if (!(await readDeviceForVerify())) return;

    let bad = 0;
    let stale = 0;
    let checked = 0;
    for (const entry of state.plan.entries) {
      const { installed, verdict } = entry;
      if (!installed || verdict === 'absent') continue;

      checked++;
      if (verdict === 'other-version') {
        // Worth naming both versions rather than saying "other": this is the
        // line that tells you an install did not reach flash, and "0.1.0 on the
        // watch, 0.2.0 selected" says that where "OTHER VERSION" does not.
        stale++;
        log(
          `  [${entry.installed.version} on watch, ${versionLabel(entry.app)} selected] ` +
            `${installed.folder}/${installed.file}`,
          'bad',
        );
        continue;
      }
      const [text, cls, counts] = VERDICTS[verdict] ?? [verdict, 'bad', true];
      log(`  [${text}] ${installed.folder}/${installed.file}`, cls);
      if (counts) bad++;
    }

    log('');
    if (checked === 0) log('Nothing from the catalogue is installed yet.');
    else if (bad === 0 && stale === 0) log(`All ${checked} file(s) verified against flash.`, 'ok');
    if (stale > 0) {
      log(
        `${stale} app(s) are not the version selected. If you have just installed them, ` +
          'the write did not reach flash — eject the drive properly and install again.',
        'bad',
      );
    }
    if (bad > 0) log(`${bad} file(s) failed — re-install, eject, reconnect, verify.`, 'bad');
  } finally {
    setBusy(false);
  }
}

// ----------------------------------------------------------------- plan render

/**
 * Entries that would be written, in the catalogue's own grouping order so the two
 * lists read consistently.
 *
 * `isActionable` comes from the planner rather than being re-derived from the
 * status here: a corrupt install is actionable, and inferring the set from
 * `install`/`update` alone silently dropped those.
 */
function actionableJobs() {
  const order = new Map(TYPE_SECTIONS.map((s, i) => [s.type, i]));
  return (state.plan?.entries ?? [])
    .filter((e) => e.isActionable)
    .sort(
      (a, b) =>
        (order.get(a.app.type) ?? 99) - (order.get(b.app.type) ?? 99) ||
        a.app.name.localeCompare(b.app.name),
    );
}

/** The jobs actually ticked, which is what any install or script must cover. */
function selectedJobs() {
  return actionableJobs().filter((e) => !state.excluded.has(e.app.appId));
}

/**
 * Set the ticks from a predicate, then re-render.
 *
 * "Updates only" exists because installing a new app and updating one already on
 * the watch are different decisions — the vendor's own script defaults to
 * update-only for the same reason.
 */
function selectJobs(wanted) {
  state.excluded = new Set(
    actionableJobs()
      .filter((e) => !wanted(e))
      .map((e) => e.app.appId),
  );
  applyDependencies();
  renderPlan();
}

/**
 * Un-exclude whatever the ticked jobs depend on.
 *
 * A variant alias runs another app's binary, so choosing one without its target
 * buys a launcher entry with nothing behind it. Which app that is, and whether
 * this plan would supply a build new enough to satisfy the descriptor's floor,
 * are the planner's to answer — this only puts the tick where it says.
 *
 * Applied to the visible selection rather than silently at install time, on
 * purpose: a selection that quietly grew is exactly the surprise the per-app
 * ticks exist to prevent, so the box has to be seen to move. Unticking a target
 * while the variant that needs it is still ticked therefore springs back, and
 * the variant's row says why — the alternative is letting somebody assemble a
 * selection that installs a launcher entry with nothing behind it.
 */
function applyDependencies() {
  if (!state.store || !state.plan) return;
  const wanted = new Set(
    state.store.withDependencies(
      state.installed,
      selectedJobs().map((e) => e.app.appId),
    ),
  );
  for (const id of wanted) state.excluded.delete(id);
}

/**
 * The order the writes go in, from the planner.
 *
 * A variant's target lands before the variant. It only shows in a run that stops
 * halfway — the launcher list is rebuilt at boot either way — but stopping after
 * the binary leaves nothing broken, and stopping after the alias leaves a
 * launcher entry with no binary behind it. Taken from the plan rather than
 * re-sorted here, so this path and the generated scripts cannot disagree.
 */
function inWriteOrder(jobs) {
  const order = new Map((state.plan?.writeOrder ?? []).map((id, i) => [id, i]));
  return [...jobs].sort(
    (a, b) => (order.get(a.app.appId) ?? 0) - (order.get(b.app.appId) ?? 0),
  );
}

/** Anchor for an app's card, so a plan row can point at it. */
const cardAnchor = (appId) => `app-${appId}`;

function renderPlan() {
  const panel = el('plan-section');
  const list = el('plan-list');
  const actions = el('plan-actions');
  list.textContent = '';
  actions.textContent = '';

  if (!state.plan) {
    panel.hidden = true;
    return;
  }
  panel.hidden = false;

  const plan = state.plan;
  el('plan-summary').textContent =
    `${plan.install} to install · ${plan.update} to update · ${plan.current} up to date` +
    (plan.corrupt > 0 ? ` · ${plan.corrupt} corrupt` : '') +
    (plan.restamps > 0 ? ` · ${plan.restamps} version-only` : '');

  const jobs = actionableJobs();
  const chosen = selectedJobs();
  if (jobs.length > 0) list.appendChild(renderSelectors(jobs, chosen));

  for (const entry of jobs) {
    const row = document.createElement('div');
    row.className = 'plan-row';

    const tick = document.createElement('input');
    tick.type = 'checkbox';
    tick.className = 'pick-job';
    tick.checked = !state.excluded.has(entry.app.appId);
    tick.setAttribute('aria-label', `Install ${entry.app.name}`);
    tick.addEventListener('change', () => {
      if (tick.checked) state.excluded.delete(entry.app.appId);
      else state.excluded.add(entry.app.appId);
      applyDependencies();
      renderPlan();
    });
    row.appendChild(tick);

    const left = document.createElement('div');
    left.className = 'plan-main';
    // Links to the card, which is where the version picker, the release history
    // and the provenance for this app live.
    const name = document.createElement('a');
    name.className = 'plan-name';
    name.href = `#${cardAnchor(entry.app.appId)}`;
    name.textContent = `${entry.app.name} (${entry.app.type})`;
    left.appendChild(name);
    const what = document.createElement('div');
    what.className = 'what';
    const where = entry.installed ? entry.installed.folder : entry.app.folder;
    what.textContent = `${entry.describe} · Apps/${where}/`;
    left.appendChild(what);
    // A variant alias runs another app's binary. The sentence, and whether there
    // is one to show at all, come from the planner: a satisfied dependency is not
    // news, and which app it is comes out of the descriptor rather than from here.
    if (entry.requiresNote) {
      const needs = document.createElement('div');
      needs.className = 'what needs';
      needs.textContent = entry.requiresNote;
      left.appendChild(needs);
    }
    row.appendChild(left);

    const right = document.createElement('div');
    right.className = 'plan-right';
    const size = document.createElement('span');
    size.className = 'muted';
    size.textContent = fmtSize(entry.app.size);
    right.appendChild(size);
    // The exact binary this row plans to write, so the row is useful even where
    // the page cannot do the writing itself.
    const dl = document.createElement('a');
    dl.className = 'dl';
    dl.href = `${DATA_BASE}/${entry.app.download}`;
    dl.setAttribute('download', entry.app.file);
    dl.textContent = 'Download';
    dl.title = `${entry.app.file} → Apps/${where}/`;
    right.appendChild(dl);
    row.appendChild(right);

    list.appendChild(row);
  }

  for (const entry of plan.entries) {
    if (entry.installed?.extraUapps?.length) {
      const warn = document.createElement('p');
      warn.className = 'note';
      warn.textContent =
        `Apps/${entry.installed.folder}/ holds more than one .uapp ` +
        `(${[entry.installed.file, ...entry.installed.extraUapps].join(', ')}). ` +
        'The watch loads whichever it finds first, so it may be running the older build. ' +
        'Installing here will clean that up.';
      list.appendChild(warn);
    }
  }

  // Apps in the way of an install, named so the folder clash is fixable. The
  // planner keys this on the folder, not the id, which is what makes it catch an
  // app the catalogue has never heard of.
  for (const entry of plan.entries.filter((e) => e.status === 'folder-taken')) {
    const warn = document.createElement('p');
    warn.className = 'note';
    warn.textContent =
      `${entry.app.name} cannot be installed: ${entry.describe}. ` +
      'Installing clears other .uapp files from the folder it writes to, so Kira ' +
      'will not touch it. Remove that app from the watch if you want this one.';
    list.appendChild(warn);
  }

  if (plan.foreign.length > 0) {
    const note = document.createElement('p');
    note.className = 'muted';
    // A variant made on the watch can never be in a catalogue -- it ships in no
    // release -- so `describe` is the only thing that can say what it is. The
    // planner decides whether there is anything to add; the folder alone is all
    // there is for everything else.
    const named = plan.foreign.map((f) =>
      f.describe ? `${f.installed.folder} — ${f.describe}` : f.installed.folder,
    );
    note.textContent =
      `${plan.foreign.length} app(s) on the watch are not in this catalogue ` +
      `(${named.join(', ')}). Kira leaves them alone.`;
    list.appendChild(note);
  }

  if (jobs.length === 0) {
    const note = document.createElement('p');
    note.className = 'muted';
    note.textContent = 'Everything selected is already installed and up to date.';
    list.appendChild(note);
    el('script-details').hidden = true;
    return;
  }

  const count = chosen.length;
  const plural = count === 1 ? '' : 's';
  if (state.mode === 'write') {
    const go = document.createElement('button');
    go.className = 'primary';
    go.type = 'button';
    go.disabled = count === 0;
    go.textContent = count === 0 ? 'Nothing selected' : `Install ${count} app${plural}`;
    go.addEventListener('click', () => void installAll());
    actions.appendChild(go);
    el('script-details').hidden = true;
  } else {
    // This tier cannot write to the drive, so the script *is* the install-all
    // action. Promote it out of the collapsed preview, which read as an
    // afterthought next to a list with nothing to click.
    const go = document.createElement('button');
    go.className = 'primary';
    go.type = 'button';
    go.disabled = count === 0;
    go.textContent =
      count === 0 ? 'Nothing selected' : `Download installer for ${count} app${plural}`;
    go.addEventListener('click', downloadScript);
    actions.appendChild(go);

    const why = document.createElement('p');
    why.className = 'muted';
    why.textContent =
      'Run it and it performs exactly these writes, checking each binary before it ' +
      'touches the watch and reading it back afterwards before removing anything. ' +
      'Then eject, reconnect, and pick the Apps folder again — that scan reads cold ' +
      'flash and is how you verify what landed. Individual binaries can be ' +
      'downloaded per app above, and each row shows the folder it belongs in.';
    // Into the list, which is cleared each render; a sibling of `actions` would
    // accumulate one copy per refresh.
    list.prepend(why);

    renderScript();
    el('script-details').hidden = false;
    el('script-details').open = false;
  }
}

/** The row of quick selectors above the pending list. */
function renderSelectors(jobs, chosen) {
  const bar = document.createElement('div');
  bar.className = 'plan-picks';

  const label = document.createElement('span');
  label.className = 'muted';
  label.textContent = `${chosen.length} of ${jobs.length} selected`;
  bar.appendChild(label);

  // "New" and "Updates" are the split worth offering: adding an app to the watch
  // is a different decision from moving one already on it forward.
  const choices = [
    ['All', () => true],
    ['Updates only', (e) => e.status !== 'install'],
    ['New only', (e) => e.status === 'install'],
    ['None', () => false],
  ];
  for (const [text, wanted] of choices) {
    if (text !== 'All' && text !== 'None' && !jobs.some(wanted)) continue;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'link';
    button.textContent = text;
    button.addEventListener('click', () => selectJobs(wanted));
    bar.appendChild(button);
  }
  return bar;
}

function currentScript() {
  const kind = el('script-kind').value === 'ps1' ? 'powershell' : 'shell';
  // The script has to do what the page says it will, so the selection goes with
  // it rather than the script quietly covering everything on offer.
  return state.store.script(
    kind,
    state.installed,
    DATA_BASE,
    selectedJobs().map((e) => e.app.appId),
  );
}

function renderScript() {
  el('script-body').textContent = currentScript();
}

function downloadScript() {
  const ps = el('script-kind').value === 'ps1';
  const blob = new Blob([currentScript()], { type: 'text/plain' });
  const link = document.createElement('a');
  link.href = URL.createObjectURL(blob);
  link.download = ps ? 'kira-install.ps1' : 'kira-install.sh';
  link.click();
  URL.revokeObjectURL(link.href);
}

// ------------------------------------------------------------------ connecting

function setBusy(busy) {
  for (const id of ['pick', 'reconnect', 'verify', 'forget']) {
    const node = el(id);
    if (!node.hidden) node.disabled = busy;
  }
}

/**
 * Read each installed app once and record what identifies it.
 *
 * Three answers come out of the same read: the whole-file hash, which says *which
 * build* is installed; the payload hash, which tells a real change from a
 * release-tag bump; and whether the CRC checks out, which separates a corrupt
 * install from a merely unfamiliar one. A header scan cannot answer any of those.
 *
 * That is a couple of megabytes read off the watch per connect, which is the
 * price of classifying rather than guessing.
 */
async function hashInstalled() {
  for (const app of state.installed) {
    if (app.sha256) continue;
    try {
      const bytes = await readInstalledBytes(app);
      app.sha256 = await sha256Hex(bytes);
      app.payloadSha256 = await payloadHash(bytes);
      app.crcValid = crcIsValid(bytes);
    } catch (err) {
      // Non-fatal: the planner falls back to what the header alone showed.
      log(`  could not read ${app.folder}/${app.file}: ${err.message}`);
    }
  }
}

async function refreshInventory() {
  if (state.mode === 'write') {
    state.installed = await readInstalledFromHandles(state.appsDir);
  }
  await hashInstalled();
  state.plan = state.store.plan(state.installed);

  renderPlan();
  renderCatalogue();
}

async function useRoot(root) {
  clearLog();
  state.appsDir = await resolveAppsDir(root);
  state.mode = 'write';
  state.blobs.clear();
  // A different watch has different settings on it, and the same watch may have
  // been edited elsewhere since. Re-read rather than trust what was prefilled.
  state.configLoaded.clear();
  await idbSet('watch', root);

  el('source').textContent = `${root.name} · read/write`;
  el('verify').hidden = false;
  el('forget').hidden = false;
  el('reconnect').hidden = true;
  el('pick').classList.replace('ghost', 'primary');
  el('pick').textContent = 'Re-scan watch';

  await refreshInventory();
  log(`Found ${state.installed.length} installed app(s) in Apps/.`);
}

/** Returns false when the picker was dismissed rather than a folder chosen. */
async function connectWithPicker() {
  let root;
  try {
    root = await window.showDirectoryPicker({ id: 'una-watch', mode: 'readwrite' });
  } catch (err) {
    if (err.name === 'AbortError') return false;
    throw err;
  }
  await useRoot(root);
  return true;
}

/**
 * Reuse a previously granted handle, so a reload does not mean picking the
 * folder again.
 *
 * The handle survives in IndexedDB, but the *permission* to use it may not:
 * Chromium drops it once every tab on the origin has closed, unless the site is
 * installed, in which case the user can grant it for every visit. Asking again
 * requires a user gesture, and page load is not one — so a still-granted handle
 * is reopened silently, and a lapsed one becomes a single button rather than a
 * failure or a full directory picker.
 */
async function tryRestore() {
  if (!CAN_WRITE) return;
  const root = await idbGet('watch').catch(() => null);
  if (!root) return;

  let permission;
  try {
    permission = await root.queryPermission({ mode: 'readwrite' });
  } catch {
    // Not a usable handle any more; do not keep offering it.
    void idbDelete('watch');
    return;
  }

  if (permission === 'granted') {
    try {
      await useRoot(root);
      return;
    } catch (err) {
      log(`Could not reopen ${root.name} (${err.message}). Is the watch connected?`);
      return;
    }
  }
  offerReconnect(root);
}

/** Offer a one-click reconnect, which supplies the gesture the prompt needs. */
function offerReconnect(root) {
  const button = el('reconnect');
  button.textContent = `Reconnect ${root.name}`;
  button.hidden = false;
  // Reconnecting is the recommended action while the offer stands; picking a
  // different folder stays available, just not as the emphasised one.
  el('pick').classList.replace('primary', 'ghost');
  button.onclick = () => {
    setBusy(true);
    reconnect(root)
      .catch((err) => log(err.message, 'bad'))
      .finally(() => setBusy(false));
  };
}

async function reconnect(root) {
  const permission = await root.requestPermission({ mode: 'readwrite' });
  if (permission !== 'granted') {
    log('Permission declined. Use Connect watch to pick the drive instead.');
    return;
  }
  await useRoot(root);
}

async function connectWithInput(files) {
  clearLog();
  state.mode = 'read';
  state.appsDir = null;
  state.installed = await readInstalledFromFiles(files);
  el('source').textContent = 'read-only snapshot';
  // No Verify button here. This tier holds a snapshot of the folder as it was
  // when it was picked, not a handle it can re-read, so the only honest way to
  // check flash is to eject, reconnect and pick again — which re-runs the scan
  // and re-plans. Offering a button that can only report on stale bytes is how
  // it came to print "All N file(s) verified against flash" having read nothing.
  el('verify').hidden = true;
  el('forget').hidden = false;
  await refreshInventory();
  log(`Found ${state.installed.length} installed app(s).`);
  if (state.installed.length === 0) {
    log("No .uapp files found — did you pick the watch's Apps folder?", 'bad');
  }
}

function disconnect() {
  state.mode = null;
  state.appsDir = null;
  state.installed = [];
  state.plan = null;
  state.blobs.clear();
  // Ticks belong to the watch that was connected, not to the next one, and
  // neither does anything read off it or typed for it.
  state.excluded.clear();
  state.configDraft.clear();
  state.configLoaded.clear();
  el('source').textContent = '';
  el('verify').hidden = true;
  el('forget').hidden = true;
  el('reconnect').hidden = true;
  el('pick').classList.replace('ghost', 'primary');
  el('pick').textContent = 'Connect watch';
  el('pick-input').value = '';
  clearLog();
  renderPlan();
  renderCatalogue();
  void idbDelete('watch');
}

// ------------------------------------------------------------------------ init

function wireUp() {
  if (CAN_WRITE) {
    el('pick').hidden = false;
    el('pick').addEventListener('click', () => {
      setBusy(true);
      connectWithPicker()
        .catch((err) => log(err.message, 'bad'))
        .finally(() => setBusy(false));
    });
  } else {
    el('pick-fallback').hidden = false;
    el('readonly-note').hidden = false;
    el('pick-input').addEventListener('change', (event) => {
      const { files } = event.target;
      if (files?.length) {
        connectWithInput(files).catch((err) => log(err.message, 'bad'));
      }
    });
  }

  el('verify').addEventListener('click', () => {
    verifyFlash().catch((err) => log(err.message, 'bad'));
  });
  el('forget').addEventListener('click', disconnect);

  const kindSelect = el('script-kind');
  kindSelect.value = detectScriptKind();
  for (const option of kindSelect.options) {
    if (option.value === kindSelect.value) option.textContent += ' — detected';
  }
  kindSelect.addEventListener('change', renderScript);
  el('script-copy').addEventListener('click', async () => {
    await navigator.clipboard.writeText(currentScript());
    el('script-copy').textContent = 'Copied';
    setTimeout(() => {
      el('script-copy').textContent = 'Copy';
    }, 1200);
  });
  el('script-download').addEventListener('click', downloadScript);
}

async function main() {
  // Catalogue first: its schema decides which module the page needs, and asking
  // for the wrong one is the failure this ordering exists to prevent.
  let catalog;
  try {
    catalog = await fetchCatalog();
  } catch (err) {
    el('catalogue').textContent = `Could not load the catalogue: ${err.message}`;
    return;
  }

  try {
    await loadModule(catalog.schema);
  } catch (err) {
    el('catalogue').textContent = `Could not load the WebAssembly module: ${err.message}`;
    return;
  }

  wireUp();
  try {
    startCatalog(catalog);
    renderCatalogue();
  } catch (err) {
    el('catalogue').textContent = `Could not load the catalogue: ${err.message}`;
    return;
  }
  await tryRestore();
}

void main();
