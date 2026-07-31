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

import init, {
  Store,
  crc_is_valid as crcIsValid,
  payload_bounds as payloadBounds,
  read_header as readHeader,
} from './lib/kira_wasm.js';

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

/** Hash the code within a .uapp, excluding the version stamp and CRC footer. */
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

async function loadCatalog() {
  const res = await fetch(`${DATA_BASE}/catalog.json`, { cache: 'no-cache' });
  if (!res.ok) throw new Error(`catalog.json: HTTP ${res.status}`);
  // Parsing is left to the browser's own JSON parser and handed over as an
  // object: linking a second JSON parser into the WebAssembly module cost about
  // 16 kB gzipped for no benefit.
  // The store validates the schema and throws if it is not the expected one.
  state.store = new Store(await res.json());

  const when = new Date(state.store.generated).toLocaleDateString();
  el('catalogue-meta').textContent =
    `${state.store.appCount} apps · ${state.store.versionCount} versions · ` +
    `${state.store.releaseCount} releases · built ${when}`;
  renderReleaseNotes();
}

/**
 * Upstream release bodies, rendered as text.
 *
 * Deliberately not parsed as Markdown or injected as HTML: this is third-party
 * content from another project's releases.
 */
function renderReleaseNotes() {
  const root = el('release-notes');
  root.textContent = '';

  for (const release of state.store.releases()) {
    const box = document.createElement('details');
    const summary = document.createElement('summary');
    const date = release.publishedAt
      ? new Date(release.publishedAt).toLocaleDateString()
      : 'date unknown';
    summary.textContent = `${release.tag} · ${date} · ${release.appCount} apps`;
    box.appendChild(summary);

    const body = document.createElement('pre');
    body.className = 'notes';
    body.textContent = release.notes || 'No release notes published upstream.';
    box.appendChild(body);

    if (release.url) {
      const link = document.createElement('a');
      link.className = 'dl';
      link.href = release.url;
      link.rel = 'noopener noreferrer';
      link.target = '_blank';
      link.textContent = 'Upstream release →';
      box.appendChild(link);
    }
    root.appendChild(box);
  }
}

function statusLabel(entry) {
  switch (entry.status) {
    case 'install':
      return ['Not installed', 'install'];
    case 'update':
      // A version-only bump is not a neutral "update" — saying so would imply
      // new code that is not there. Deliberately not styled as attention-worthy.
      return entry.identicalPayload
        ? [`${entry.installed.version} → ${entry.app.version} · same code`, '']
        : [`Update ${entry.installed.version} → ${entry.app.version}`, 'update'];
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

function renderCard(app, entry) {
  const selected = app.versions.find((v) => v.version === app.selected) ?? app.versions[0];

  const card = document.createElement('div');
  card.className = 'card';

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

  // Who built the binary on offer. For a Kira build the recipe is shown on hover,
  // since that plus the published inputs is what makes it reproducible.
  const provenance = document.createElement('div');
  provenance.className = 'meta provenance';
  if (selected.origin === 'kira') {
    provenance.textContent = 'built by Kira from source';
    const built = selected.builtFrom;
    if (built) {
      provenance.title =
        `recipe ${built.recipe}\nsource ${built.appSource}\ntoolchain ${built.toolchain}`;
    }
  } else {
    provenance.textContent = "the vendor's own build";
  }
  body.appendChild(provenance);

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
      option.value = version.version;
      const tags = [];
      if (version.version === app.versions[0].version) tags.push('latest');
      if (version.changed === false) tags.push('same code');
      option.textContent = tags.length
        ? `${version.version} · ${tags.join(' · ')}`
        : version.version;
      option.selected = version.version === app.selected;
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
  } else {
    const dl = document.createElement('a');
    dl.className = 'dl';
    dl.href = `${DATA_BASE}/${selected.download}`;
    dl.textContent = `Download ${selected.version}`;
    dl.setAttribute('download', selected.file);
    body.appendChild(dl);
  }

  card.appendChild(body);
  return card;
}

function renderCatalogue() {
  const root = el('catalogue');
  root.removeAttribute('aria-busy');
  root.textContent = '';

  const all = state.store.apps();
  // Superseded identities are listed separately: they cannot be installed, and
  // leaving them in the grids invites a misclick on an app that upstream replaced.
  const apps = all.filter((a) => !a.supersededBy);
  const archived = all.filter((a) => a.supersededBy);
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
 * Apps whose identity upstream replaced.
 *
 * Collapsed by default and kept out of the grids above: they are not installable,
 * because another app owns the folder they would be written to, and the versions
 * they carry are ancient. Still listed so the reassignment is visible and the
 * binaries remain downloadable.
 */
function renderArchive(root, archived, byId) {
  const box = document.createElement('details');
  box.className = 'archive';

  const summary = document.createElement('summary');
  summary.textContent = `Archived — ${archived.length} replaced identit${
    archived.length === 1 ? 'y' : 'ies'
  }`;
  box.appendChild(summary);

  const note = document.createElement('p');
  note.className = 'type-blurb';
  note.textContent =
    'Upstream reassigned these apps to new AppIDs. The current versions are listed ' +
    'above under the same names; these entries keep the old identity and cannot be ' +
    'installed, since the newer app owns the same folder on the watch.';
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
function installedFrom(header, folder, file, size, extraUapps) {
  return {
    appId: header.appId,
    folder,
    file,
    name: header.name,
    version: header.version,
    size,
    extraUapps,
    // Filled by hashInstalled() once the file has been read.
    payloadSha256: null,
    sha256: null,
    crcValid: null,
  };
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
  log(`${app.name} ${app.version} → Apps/${app.folder}/${app.file}`);

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
    await dir.removeEntry(app.file).catch(() => {});
    throw new Error(`short write (${back.size}/${bytes.length}); stale binary left in place`);
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
  const jobs = state.plan.entries.filter((e) => e.status === 'install' || e.status === 'update');
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
    failed === 0 ? `${jobs.length} app(s) written.` : `${failed} of ${jobs.length} failed.`,
    failed === 0 ? 'ok' : 'bad',
  );
  log('NEXT: eject the watch, reconnect it, then press "Verify flash".');
  log('Then reboot the watch — the launcher list is rebuilt only at boot.');

  await refreshInventory();
  setBusy(false);
}

// --------------------------------------------------------------------- verify

/**
 * Hash what is actually on the device against the catalogue.
 *
 * Only meaningful after an eject and reconnect: before that this reads the OS
 * write cache and can report a false OK.
 */
async function verifyFlash() {
  clearLog();
  setBusy(true);
  log('Verifying — this is only trustworthy if you ejected and reconnected the watch.');

  let bad = 0;
  let checked = 0;
  for (const entry of state.plan.entries) {
    const { app, installed } = entry;
    if (!installed) continue;

    if (installed.file !== app.file) {
      log(`  [stale   ] ${installed.folder}/${installed.file} (expected ${app.file})`, 'bad');
      bad++;
      continue;
    }

    const bytes = await readInstalledBytes(installed);
    checked++;
    if ((await sha256Hex(bytes)) === app.sha256) {
      log(`  [ok      ] ${installed.folder}/${installed.file}`, 'ok');
    } else {
      log(`  [MISMATCH] ${installed.folder}/${installed.file}`, 'bad');
      bad++;
    }
  }

  log('');
  if (checked === 0) log('Nothing from the catalogue is installed yet.');
  else if (bad === 0) log(`All ${checked} file(s) match the catalogue.`, 'ok');
  else log(`${bad} file(s) failed — re-install, eject, reconnect and verify again.`, 'bad');
  setBusy(false);
}

// ----------------------------------------------------------------- plan render

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
    (plan.restamps > 0 ? ` · ${plan.restamps} version-only` : '');

  // Same grouping order as the catalogue, so the two lists read consistently.
  const order = new Map(TYPE_SECTIONS.map((s, i) => [s.type, i]));
  const jobs = plan.entries
    .filter((e) => e.status === 'install' || e.status === 'update')
    .sort(
      (a, b) =>
        (order.get(a.app.type) ?? 99) - (order.get(b.app.type) ?? 99) ||
        a.app.name.localeCompare(b.app.name),
    );

  for (const entry of jobs) {
    const row = document.createElement('div');
    row.className = 'plan-row';

    const left = document.createElement('div');
    const name = document.createElement('strong');
    name.textContent = `${entry.app.name} (${entry.app.type})`;
    left.appendChild(name);
    const what = document.createElement('div');
    what.className = 'what';
    const where = entry.installed ? entry.installed.folder : entry.app.folder;
    what.textContent = `${entry.describe} · Apps/${where}/`;
    left.appendChild(what);
    row.appendChild(left);

    const size = document.createElement('div');
    size.className = 'muted';
    size.textContent = fmtSize(entry.app.size);
    row.appendChild(size);

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

  if (plan.foreign.length > 0) {
    const note = document.createElement('p');
    note.className = 'muted';
    note.textContent =
      `${plan.foreign.length} app(s) on the watch are not in this catalogue ` +
      `(${plan.foreign.map((f) => f.folder).join(', ')}). Kira leaves them alone.`;
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

  if (state.mode === 'write') {
    const go = document.createElement('button');
    go.className = 'primary';
    go.type = 'button';
    go.textContent = `Install ${jobs.length} app${jobs.length === 1 ? '' : 's'}`;
    go.addEventListener('click', () => void installAll());
    actions.appendChild(go);
    el('script-details').hidden = true;
  } else {
    renderScript();
    el('script-details').hidden = false;
    el('script-details').open = true;
  }
}

function currentScript() {
  const kind = el('script-kind').value === 'ps1' ? 'powershell' : 'shell';
  return state.store.script(kind, state.installed, DATA_BASE);
}

function renderScript() {
  el('script-body').textContent = currentScript();
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

async function connectWithPicker() {
  let root;
  try {
    root = await window.showDirectoryPicker({ id: 'una-watch', mode: 'readwrite' });
  } catch (err) {
    if (err.name === 'AbortError') return;
    throw err;
  }
  await useRoot(root);
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
  el('verify').hidden = state.installed.length === 0;
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
  el('script-download').addEventListener('click', () => {
    const ps = el('script-kind').value === 'ps1';
    const blob = new Blob([currentScript()], { type: 'text/plain' });
    const link = document.createElement('a');
    link.href = URL.createObjectURL(blob);
    link.download = ps ? 'kira-install.ps1' : 'kira-install.sh';
    link.click();
    URL.revokeObjectURL(link.href);
  });
}

async function main() {
  try {
    await init();
  } catch (err) {
    el('catalogue').textContent = `Could not load the WebAssembly module: ${err.message}`;
    return;
  }

  wireUp();
  try {
    await loadCatalog();
    renderCatalogue();
  } catch (err) {
    el('catalogue').textContent = `Could not load the catalogue: ${err.message}`;
    return;
  }
  await tryRestore();
}

void main();
