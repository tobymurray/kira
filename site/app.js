/**
 * Kira — browser front end.
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

import { HEADER_SIZE, parseHeader, payloadOf, verifyCrc } from './lib/uapp.js';
import {
  actionable,
  buildPlan,
  describeJob,
  needsPayloadHash,
  powershellScript,
  shellScript,
} from './lib/plan.js';
import { SCHEMA, describeHistory, latestOf, resolveTargets } from './lib/catalog.js';

const CAN_WRITE = typeof window.showDirectoryPicker === 'function';
const DATA_BASE = new URL('data', location.href).href.replace(/\/$/, '');

/** Not an app: the SDK's own docs note SharedData lives alongside app folders. */
const NON_APP_DIRS = new Set(['sharedata', 'shareddata', 'system', '.trashes', '.spotlight-v100']);

const el = (id) => document.getElementById(id);
const state = {
  catalog: null,
  /** 'write' | 'read' | null */
  mode: null,
  appsDir: null, // FileSystemDirectoryHandle, write mode only
  installed: [],
  plan: null,
  /** appId -> version, for anything the user pinned away from the newest. */
  pinned: new Map(),
  /** One chosen version per app, flattened for the planner. */
  targets: [],
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

async function idbSet(key, value) {
  const db = await idb();
  await new Promise((resolve, reject) => {
    const tx = db.transaction(IDB_STORE, 'readwrite');
    tx.objectStore(IDB_STORE).put(value, key);
    tx.oncomplete = resolve;
    tx.onerror = () => reject(tx.error);
  });
  db.close();
}

async function idbGet(key) {
  const db = await idb();
  const value = await new Promise((resolve, reject) => {
    const tx = db.transaction(IDB_STORE, 'readonly');
    const req = tx.objectStore(IDB_STORE).get(key);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
  db.close();
  return value;
}

async function idbDelete(key) {
  const db = await idb();
  await new Promise((resolve) => {
    const tx = db.transaction(IDB_STORE, 'readwrite');
    tx.objectStore(IDB_STORE).delete(key);
    tx.oncomplete = resolve;
  });
  db.close();
}

// ------------------------------------------------------------------ catalogue

async function loadCatalog() {
  const res = await fetch(`${DATA_BASE}/catalog.json`, { cache: 'no-cache' });
  if (!res.ok) throw new Error(`catalog.json: HTTP ${res.status}`);
  const catalog = await res.json();
  if (catalog.schema !== SCHEMA) {
    throw new Error(`unsupported catalogue schema ${catalog.schema} (expected ${SCHEMA})`);
  }
  state.catalog = catalog;
  retarget();

  const versions = catalog.apps.reduce((n, a) => n + a.versions.length, 0);
  const when = new Date(catalog.generated).toLocaleDateString();
  el('catalogue-meta').textContent =
    `${catalog.apps.length} apps · ${versions} versions · ` +
    `${catalog.releases.length} releases · built ${when}`;
  renderReleaseNotes();
  return catalog;
}

/** Re-resolve which version of each app is selected. */
function retarget() {
  state.targets = resolveTargets(state.catalog, state.pinned);
}

/** The version currently selected for an app. */
function targetFor(appId) {
  return state.targets.find((t) => t.appId === appId);
}

/**
 * Upstream release bodies, rendered as text.
 *
 * Deliberately not parsed as Markdown or injected as HTML: this is third-party
 * content fetched from another project's releases.
 */
function renderReleaseNotes() {
  const root = el('release-notes');
  root.textContent = '';

  for (const release of state.catalog.releases) {
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

function statusLabel(status, entry) {
  switch (status) {
    case 'install':
      return ['Not installed', 'install'];
    case 'update':
      // A version-only bump is not a neutral "update" — saying so would imply
      // new code that is not there. Deliberately not styled as attention-worthy.
      return entry.identicalPayload
        ? [`${entry.installed.version} → ${entry.app.version} · same code`, '']
        : [`Update ${entry.installed.version} → ${entry.app.version}`, 'update'];
    case 'current':
      return ['Up to date', 'current'];
    case 'newer-on-watch':
      return [`Watch has ${entry.installed.version}`, ''];
    default:
      return [status, ''];
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
  {
    type: 'Utility',
    heading: 'Utilities',
    blurb: 'Standalone apps you open from the launcher.',
  },
  {
    type: 'Glance',
    heading: 'Glances',
    blurb: 'Compact widgets for the 240×60 notification area, not full-screen apps.',
  },
  {
    type: 'Clockface',
    heading: 'Clockfaces',
    blurb: 'Watch faces.',
  },
];

/**
 * @param {object} app     catalogue record, with the full version list
 * @param {object} target  the version currently selected for this app
 * @param {object} [entry] plan entry, when a watch is connected
 */
function renderCard(app, target, entry) {
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

  const h3 = document.createElement('h3');
  h3.textContent = app.name;
  body.appendChild(h3);

  const meta = document.createElement('div');
  meta.className = 'meta';
  // The type is the section heading here, so the card need not repeat it.
  meta.textContent =
    `${fmtSize(target.size)}${target.autostart ? ' · autostarts' : ''} · ${describeHistory(app)}`;
  body.appendChild(meta);

  // Upstream has reassigned AppIDs: three Glances carry one ID up to
  // apps-v0.1.9-rc1 and a different one after. Those are separate identities as
  // far as the watch and the phone are concerned, so they stay separate entries
  // — but say which is which rather than showing two identical-looking cards.
  if (duplicateNames.has(app.name)) {
    const id = document.createElement('div');
    id.className = 'meta appid';
    id.textContent = `AppID ${app.appId}`;
    id.title = 'Another entry shares this name under a different AppID';
    body.appendChild(id);
  }

  // Upstream offers no way to fetch a specific build, so every published
  // version is selectable here. Newest is the default.
  if (app.versions.length > 1) {
    const picker = document.createElement('select');
    picker.className = 'version';
    picker.setAttribute('aria-label', `Version of ${app.name}`);
    for (const v of app.versions) {
      const option = document.createElement('option');
      option.value = v.version;
      const tags = [];
      if (v.version === latestOf(app).version) tags.push('latest');
      if (v.changed === false) tags.push('same code');
      option.textContent = tags.length ? `${v.version} · ${tags.join(' · ')}` : v.version;
      option.selected = v.version === target.version;
      picker.appendChild(option);
    }
    picker.addEventListener('change', () => void pinVersion(app, picker.value));
    body.appendChild(picker);
  }

  if (entry) {
    const [text, cls] = statusLabel(entry.status, entry);
    const badge = document.createElement('span');
    badge.className = `badge ${cls}`;
    badge.textContent = text;
    body.appendChild(badge);
  } else {
    const dl = document.createElement('a');
    dl.className = 'dl';
    dl.href = `${DATA_BASE}/${target.download}`;
    dl.textContent = `Download ${target.version}`;
    dl.setAttribute('download', target.file);
    body.appendChild(dl);
  }

  card.appendChild(body);
  return card;
}

/** Pin an app to a specific version, or back to newest, and re-plan. */
async function pinVersion(app, version) {
  if (version === latestOf(app).version) state.pinned.delete(app.appId);
  else state.pinned.set(app.appId, version);
  retarget();

  // The chosen version changes what counts as an update, and its payload hash
  // has to be compared against the watch again.
  if (state.mode) await refreshInventory();
  else renderCatalogue();
}

/** Display names shared by more than one AppID, so cards can disambiguate. */
let duplicateNames = new Set();

function renderCatalogue() {
  const root = el('catalogue');
  root.removeAttribute('aria-busy');
  root.textContent = '';

  const counts = new Map();
  for (const a of state.catalog.apps) counts.set(a.name, (counts.get(a.name) ?? 0) + 1);
  duplicateNames = new Set([...counts].filter(([, n]) => n > 1).map(([name]) => name));

  const byId = new Map((state.plan?.entries ?? []).map((e) => [e.app.appId, e]));
  const seen = new Set();

  for (const section of TYPE_SECTIONS) {
    const apps = state.catalog.apps.filter((a) => a.type === section.type);
    apps.forEach((a) => seen.add(a.appId));
    if (apps.length === 0) continue;

    const group = document.createElement('section');
    group.className = 'type-group';

    const head = document.createElement('div');
    head.className = 'type-head';
    const h3 = document.createElement('h3');
    h3.textContent = section.heading;
    head.appendChild(h3);
    const count = document.createElement('span');
    count.className = 'muted';
    count.textContent = `${apps.length}`;
    head.appendChild(count);
    group.appendChild(head);

    const blurb = document.createElement('p');
    blurb.className = 'type-blurb';
    blurb.textContent = section.blurb;
    group.appendChild(blurb);

    const grid = document.createElement('div');
    grid.className = 'grid';
    for (const app of apps.sort((a, b) => a.name.localeCompare(b.name))) {
      grid.appendChild(renderCard(app, targetFor(app.appId), byId.get(app.appId)));
    }
    group.appendChild(grid);
    root.appendChild(group);
  }

  // Anything with a type this build does not know about still gets shown.
  const rest = state.catalog.apps.filter((a) => !seen.has(a.appId));
  if (rest.length > 0) {
    const group = document.createElement('section');
    group.className = 'type-group';
    const h3 = document.createElement('h3');
    h3.textContent = 'Other';
    group.appendChild(h3);
    const grid = document.createElement('div');
    grid.className = 'grid';
    for (const app of rest) {
      grid.appendChild(renderCard(app, targetFor(app.appId), byId.get(app.appId)));
    }
    group.appendChild(grid);
    root.appendChild(group);
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
      const head = new Uint8Array(await file.slice(0, HEADER_SIZE).arrayBuffer());
      const header = parseHeader(head, file.size);
      installed.push({
        appId: header.appId,
        folder: name,
        file: fileName,
        name: header.name,
        version: header.version,
        versionPacked: header.appVersion,
        size: file.size,
        extraUapps: uapps.slice(1).map((u) => u.fileName),
      });
    } catch (err) {
      log(`  ${name}/${fileName}: not a readable .uapp (${err.message})`, 'bad');
    }
  }
  return installed;
}

/** Same, from a <input webkitdirectory> FileList. */
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
  for (const [folder, group] of [...byFolder].sort((a, b) => a[0].localeCompare(b[0]))) {
    group.sort((a, b) => a.name.localeCompare(b.name));
    const [file] = group;
    try {
      const head = new Uint8Array(await file.slice(0, HEADER_SIZE).arrayBuffer());
      const header = parseHeader(head, file.size);
      installed.push({
        appId: header.appId,
        folder,
        file: file.name,
        name: header.name,
        version: header.version,
        versionPacked: header.appVersion,
        size: file.size,
        extraUapps: group.slice(1).map((f) => f.name),
        blob: file, // kept so Verify can hash without re-picking
      });
    } catch (err) {
      log(`  ${folder}/${file.name}: not a readable .uapp (${err.message})`, 'bad');
    }
  }
  return installed;
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
  const sha = await sha256Hex(bytes);
  if (sha !== app.sha256) throw new Error('SHA-256 mismatch against the catalogue');

  // A bad CRC would be dropped silently by the kernel — the app would simply
  // never appear in the launcher. Refuse to write one.
  const crc = verifyCrc(bytes);
  if (!crc.ok) throw new Error('CRC32 footer is invalid');

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

  log(`  ok`, 'ok');
}

async function installAll() {
  const jobs = actionable(state.plan);
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
  log(failed === 0 ? `${jobs.length} app(s) written.` : `${failed} of ${jobs.length} failed.`,
    failed === 0 ? 'ok' : 'bad');
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
    const expected = targetFor(app.appId);
    if (installed.file !== expected.file) {
      log(`  [stale   ] ${installed.folder}/${installed.file} (expected ${expected.file})`, 'bad');
      bad++;
      continue;
    }

    const bytes = await readInstalledBytes(installed);

    checked++;
    const sha = await sha256Hex(bytes);
    if (sha === expected.sha256) {
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
  const section = el('plan-section');
  const list = el('plan-list');
  const actions = el('plan-actions');
  list.textContent = '';
  actions.textContent = '';

  if (!state.plan) {
    section.hidden = true;
    return;
  }
  section.hidden = false;

  const jobs = actionable(state.plan);
  const counts = { install: 0, update: 0, current: 0 };
  for (const e of state.plan.entries) counts[e.status] = (counts[e.status] ?? 0) + 1;
  const restamps = state.plan.entries.filter((e) => e.identicalPayload).length;
  el('plan-summary').textContent =
    `${counts.install ?? 0} to install · ${counts.update ?? 0} to update · ` +
    `${counts.current ?? 0} up to date` +
    (restamps > 0 ? ` · ${restamps} version-only` : '');

  // Same grouping order as the catalogue, so the two lists read consistently.
  const typeOrder = new Map(TYPE_SECTIONS.map((s, i) => [s.type, i]));
  const ordered = [...jobs].sort(
    (a, b) =>
      (typeOrder.get(a.app.type) ?? 99) - (typeOrder.get(b.app.type) ?? 99) ||
      a.app.name.localeCompare(b.app.name),
  );

  for (const entry of ordered) {
    const row = document.createElement('div');
    row.className = 'plan-row';

    const left = document.createElement('div');
    const name = document.createElement('strong');
    name.textContent = `${entry.app.name} (${entry.app.type})`;
    left.appendChild(name);
    const what = document.createElement('div');
    what.className = 'what';
    const where = entry.installed ? entry.installed.folder : entry.app.folder;
    what.textContent = `${describeJob(entry)} · Apps/${where}/`;
    left.appendChild(what);
    row.appendChild(left);

    const right = document.createElement('div');
    right.className = 'muted';
    right.textContent = fmtSize(entry.app.size);
    row.appendChild(right);

    list.appendChild(row);
  }

  for (const entry of state.plan.entries) {
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

  if (state.plan.foreign.length > 0) {
    const p = document.createElement('p');
    p.className = 'muted';
    p.textContent =
      `${state.plan.foreign.length} app(s) on the watch are not in this catalogue ` +
      `(${state.plan.foreign.map((f) => f.folder).join(', ')}). Kira leaves them alone.`;
    list.appendChild(p);
  }

  if (jobs.length === 0) {
    const p = document.createElement('p');
    p.className = 'muted';
    p.textContent = 'Everything in the catalogue is already installed and up to date.';
    list.appendChild(p);
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
  const kind = el('script-kind').value;
  return kind === 'ps1'
    ? powershellScript(state.plan, { baseUrl: DATA_BASE })
    : shellScript(state.plan, { baseUrl: DATA_BASE });
}

function renderScript() {
  el('script-body').textContent = currentScript();
}

// ------------------------------------------------------------------ connecting

function setBusy(busy) {
  for (const id of ['pick', 'verify', 'forget']) {
    const node = el(id);
    if (!node.hidden) node.disabled = busy;
  }
}

/** Read a whole installed .uapp, in either connection mode. */
async function readInstalledBytes(installed) {
  if (installed.blob) return new Uint8Array(await installed.blob.arrayBuffer());
  const dir = await state.appsDir.getDirectoryHandle(installed.folder);
  const handle = await dir.getFileHandle(installed.file);
  return new Uint8Array(await (await handle.getFile()).arrayBuffer());
}

/**
 * Hash the code of anything that looks like an update, so a release-tag bump can
 * be told apart from a real change.
 *
 * Only update candidates are read in full — the initial scan reads 48-byte
 * headers, and reading every app off a USB volume to answer a cosmetic question
 * would not be worth the wait.
 */
async function deepenUpdateCandidates(plan) {
  const pending = needsPayloadHash(plan);
  if (pending.length === 0) return false;

  for (const entry of pending) {
    try {
      const bytes = await readInstalledBytes(entry.installed);
      entry.installed.payloadSha256 = await sha256Hex(payloadOf(bytes));
    } catch (err) {
      // Non-fatal: without a hash the entry stays an ordinary update.
      log(`  could not hash ${entry.installed.folder}/${entry.installed.file}: ${err.message}`);
    }
  }
  return true;
}

async function refreshInventory() {
  if (state.mode === 'write') {
    state.installed = await readInstalledFromHandles(state.appsDir);
  }
  state.plan = buildPlan({ apps: state.targets }, state.installed);

  // Re-plan once the payload hashes are known, so labels reflect them.
  if (await deepenUpdateCandidates(state.plan)) {
    state.plan = buildPlan({ apps: state.targets }, state.installed);
  }

  renderPlan();
  renderCatalogue();
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

async function useRoot(root) {
  clearLog();
  const appsDir = await resolveAppsDir(root);
  state.appsDir = appsDir;
  state.mode = 'write';
  await idbSet('watch', root);

  el('source').textContent = `${root.name} · read/write`;
  el('verify').hidden = false;
  el('forget').hidden = false;
  el('pick').textContent = 'Re-scan watch';

  await refreshInventory();
  log(`Found ${state.installed.length} installed app(s) in Apps/.`);
}

/** Try to reuse a previously granted handle, so Verify survives a reconnect. */
async function tryRestore() {
  if (!CAN_WRITE) return false;
  const root = await idbGet('watch').catch(() => null);
  if (!root) return false;
  try {
    let perm = await root.queryPermission({ mode: 'readwrite' });
    if (perm === 'prompt') perm = await root.requestPermission({ mode: 'readwrite' });
    if (perm !== 'granted') return false;
    await useRoot(root);
    return true;
  } catch (err) {
    log(`Could not reopen the last watch (${err.message}). Pick it again.`);
    return false;
  }
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
    log('No .uapp files found — did you pick the watch\'s Apps folder?', 'bad');
  }
}

function disconnect() {
  state.mode = null;
  state.appsDir = null;
  state.installed = [];
  state.plan = null;
  el('source').textContent = '';
  el('verify').hidden = true;
  el('forget').hidden = true;
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

  el('script-kind').addEventListener('change', renderScript);
  el('script-copy').addEventListener('click', async () => {
    await navigator.clipboard.writeText(currentScript());
    el('script-copy').textContent = 'Copied';
    setTimeout(() => (el('script-copy').textContent = 'Copy'), 1200);
  });
  el('script-download').addEventListener('click', () => {
    const kind = el('script-kind').value;
    const blob = new Blob([currentScript()], { type: 'text/plain' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = kind === 'ps1' ? 'kira-install.ps1' : 'kira-install.sh';
    a.click();
    URL.revokeObjectURL(a.href);
  });
}

async function main() {
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
