#!/usr/bin/env node
/**
 * Build the Kira catalogue (schema 2) from one or more una-apps releases.
 *
 * Input is the release zips' own layout — one directory per app, each holding
 * exactly one .uapp — either directly under --src for a single release, or
 * nested one level under a release tag:
 *
 *   <src>/apps-v1.3.0/GlanceHR/Live_HR_1.3.0.uapp     (multi-release)
 *   <src>/GlanceHR/Live_HR_1.3.0.uapp                 (single, tag from --tag)
 *
 * The app directory name matters: it is the folder the watch expects under
 * Apps\ , and it is NOT derivable from the app's display name (GlanceARHR is
 * named "AVG / R HR", which contains a path separator). Everything else comes
 * out of the .uapp header.
 *
 * Release notes and dates come from --releases, a JSON array of
 * {tag, publishedAt, url, isPrerelease, notes}. This tool does no network I/O:
 * the workflow fetches that, so builds stay hermetic and testable.
 *
 * Also copies the shared ES modules from src/ into <out>/lib/ so the browser
 * runs the very same code as this build, with no second implementation.
 *
 * Usage:
 *   node tools/build-catalog.mjs --src <dir> --out site \
 *        [--releases releases.json] [--repo UNAWatch/una-sdk] [--tag apps-v1.3.0]
 */

import { createHash } from 'node:crypto';
import { copyFile, readdir, readFile, mkdir, writeFile, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { parseUapp, decodeIcon, isBlankIcon, payloadOf, compareVersions } from '../src/uapp.js';
import { SCHEMA, partitionByUniqueId, sortReleases } from '../src/catalog.js';
import { encodePng } from '../src/png.js';

const SHARED_MODULES = ['uapp.js', 'plan.js', 'catalog.js'];
const SRC_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', 'src');

function parseArgs(argv) {
  const args = { src: null, out: 'site', repo: null, tag: null, releases: null };
  for (let i = 2; i < argv.length; i++) {
    const key = argv[i].replace(/^--/, '');
    if (!(key in args)) throw new Error(`unknown argument: ${argv[i]}`);
    args[key] = argv[++i];
  }
  if (!args.src) throw new Error('--src <dir> is required');
  return args;
}

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

async function subdirs(dir) {
  return (await readdir(dir, { withFileTypes: true }))
    .filter((e) => e.isDirectory())
    .map((e) => e.name)
    .sort((a, b) => a.localeCompare(b));
}

/** Find <root>/<Folder>/<one>.uapp, rejecting ambiguous folders. */
async function discoverApps(root) {
  const found = [];
  for (const folder of await subdirs(root)) {
    const dir = join(root, folder);
    const uapps = (await readdir(dir)).filter((f) => f.toLowerCase().endsWith('.uapp'));
    if (uapps.length === 0) continue;
    if (uapps.length > 1) {
      // The watch loads the FIRST .uapp it finds in a folder, so shipping two
      // is never right — refuse rather than pick arbitrarily.
      throw new Error(`${folder}: ${uapps.length} .uapp files, expected 1: ${uapps.join(', ')}`);
    }
    found.push({ folder, file: uapps[0], path: join(dir, uapps[0]) });
  }
  return found;
}

/**
 * Single release directly under --src, or one directory per release tag?
 * Decided by looking for .uapp files one level down.
 */
async function detectLayout(src) {
  for (const child of await subdirs(src)) {
    const files = await readdir(join(src, child));
    if (files.some((f) => f.toLowerCase().endsWith('.uapp'))) return 'single';
  }
  return 'multi';
}

/** Parse every app in one release directory. */
async function readRelease(tag, dir) {
  const parsed = [];

  for (const { folder, file, path } of await discoverApps(dir)) {
    const bytes = new Uint8Array(await readFile(path));
    const app = parseUapp(bytes);

    // A CRC failure means the kernel would silently drop this file. Never
    // publish one: the user would install it and see nothing appear.
    if (!app.crc.ok) {
      throw new Error(
        `${tag}/${folder}/${file}: CRC mismatch (stored 0x${app.crc.stored?.toString(16)}, ` +
          `computed 0x${app.crc.computed?.toString(16)}) — refusing to publish`,
      );
    }
    parsed.push({ tag, folder, file, bytes, app });
  }

  // Within one release an AppID must be unique; across releases it repeats by
  // design, which is how versions are grouped. Older releases do contain
  // collisions — apps-v0.1.9-rc3 ships GlanceStrain and GlanceActivity under
  // the same ID — and since AppID is the identity Kira installs against, a
  // colliding binary cannot be attributed to either app without guessing.
  // Drop every side of a collision and say so, rather than guessing or letting
  // one bad historical release sink the whole catalogue.
  const { unique, collisions } = partitionByUniqueId(
    parsed,
    (e) => e.app.appId,
    (e) => e.folder,
  );
  return {
    apps: unique,
    dropped: collisions.map((c) => `${c.id} claimed by ${c.labels.join(' and ')}`),
  };
}

async function main() {
  const args = parseArgs(process.argv);
  const src = resolve(args.src);
  const site = resolve(args.out);
  const out = join(site, 'data');

  const layout = await detectLayout(src);
  const releaseDirs =
    layout === 'single'
      ? [{ tag: args.tag ?? 'unversioned', dir: src }]
      : (await subdirs(src)).map((tag) => ({ tag, dir: join(src, tag) }));

  let meta = [];
  if (args.releases) {
    meta = JSON.parse(await readFile(resolve(args.releases), 'utf8'));
    if (!Array.isArray(meta)) throw new Error('--releases must contain a JSON array');
  }
  const metaFor = new Map(meta.map((r) => [r.tag, r]));

  // Newest release first, so each app's version list is newest first too.
  const ordered = sortReleases(releaseDirs.map((r) => ({ ...r, ...metaFor.get(r.tag) })));

  await rm(join(out, 'icons'), { recursive: true, force: true });
  await rm(join(out, 'apps'), { recursive: true, force: true });
  await mkdir(join(out, 'icons'), { recursive: true });

  // One parser, two runtimes: the browser imports these copies.
  await mkdir(join(site, 'lib'), { recursive: true });
  for (const name of SHARED_MODULES) {
    await copyFile(join(SRC_DIR, name), join(site, 'lib', name));
  }

  /** appId -> catalogue entry under construction */
  const apps = new Map();
  const releases = [];
  let totalBytes = 0;

  const skipped = [];

  for (const release of ordered) {
    const { apps: entries, dropped } = await readRelease(release.tag, release.dir);
    for (const collision of dropped) {
      console.warn(`  ! ${release.tag}: dropped AppID collision — ${collision}`);
    }
    console.log(
      `${release.tag}: ${entries.length} apps` +
        (dropped.length ? ` (${dropped.length} dropped)` : ''),
    );
    if (entries.length === 0) {
      skipped.push(release.tag);
      continue;
    }

    for (const { tag, folder, file, bytes, app } of entries) {
      const download = ['apps', tag, folder, file].join('/');
      await mkdir(join(out, 'apps', tag, folder), { recursive: true });
      await writeFile(join(out, download), bytes);
      totalBytes += bytes.length;

      if (!apps.has(app.appId)) {
        apps.set(app.appId, {
          appId: app.appId,
          name: app.name,
          type: app.type,
          folder,
          versions: [],
          // Filled from the newest version that actually carries icon pixels.
          icon: undefined,
          iconSmall: undefined,
        });
      }
      const entry = apps.get(app.appId);

      // Same version published under two tags: keep the newer release's copy.
      if (entry.versions.some((v) => v.version === app.version)) continue;

      entry.versions.push({
        version: app.version,
        versionPacked: app.appVersion,
        tag,
        folder,
        file,
        libcVersion: app.libcVersionStr,
        autostart: app.autostart,
        size: bytes.length,
        sha256: sha256(bytes),
        // Hash of the code alone, with the version stamp and CRC excluded, so
        // "the code changed" can be told from "the release tag moved".
        payloadSha256: sha256(payloadOf(bytes)),
        download,
      });

      // Icons come from the newest version that has any, since a declared size
      // does not mean there are pixels: Glance apps built with icons off carry a
      // zero-filled field of the full size.
      for (const [key, offset, size, suffix] of [
        ['icon', app.normalIconOffset, app.normalIconSize, ''],
        ['iconSmall', app.smallIconOffset, app.smallIconSize, '@30'],
      ]) {
        if (entry[key] || !size) continue;
        const raw = bytes.subarray(offset, offset + size);
        if (isBlankIcon(raw)) continue;
        const { width, height, rgba } = decodeIcon(raw);
        const rel = `icons/${app.appId}${suffix}.png`;
        await writeFile(join(out, rel), encodePng(rgba, width, height));
        entry[key] = rel;
      }
    }

    releases.push({
      tag: release.tag,
      publishedAt: release.publishedAt ?? null,
      url: release.url ?? null,
      isPrerelease: release.isPrerelease ?? false,
      // Upstream release bodies, verbatim. Rendered as text by the site, never
      // as HTML — this is third-party Markdown.
      notes: typeof release.notes === 'string' ? release.notes.trim() : null,
      appCount: entries.length,
    });
  }

  // Annotate each version against the next older one: did the code move?
  for (const entry of apps.values()) {
    entry.versions.sort((a, b) => compareVersions(b.versionPacked, a.versionPacked));
    entry.versions.forEach((v, i) => {
      const older = entry.versions[i + 1];
      // null, not false: with no predecessor published here, it is unknown.
      v.changed = older ? v.payloadSha256 !== older.payloadSha256 : null;
      v.deltaBytes = older ? v.size - older.size : null;
    });
    // Present-tense metadata tracks the newest build.
    const latest = entry.versions[0];
    entry.folder = latest.folder;
  }

  const catalog = {
    schema: SCHEMA,
    generated: new Date().toISOString(),
    source: { repo: args.repo },
    releases,
    apps: [...apps.values()].sort((a, b) => a.name.localeCompare(b.name)),
  };
  await writeFile(join(out, 'catalog.json'), `${JSON.stringify(catalog, null, 2)}\n`);

  if (releases.length === 0) throw new Error('no usable releases: nothing to publish');

  const versionCount = catalog.apps.reduce((n, a) => n + a.versions.length, 0);
  const restamps = catalog.apps.reduce(
    (n, a) => n + a.versions.filter((v) => v.changed === false).length,
    0,
  );
  console.log(
    `\n${catalog.apps.length} apps · ${versionCount} versions across ${releases.length} release(s)` +
      (skipped.length ? `\nskipped (no usable apps): ${skipped.join(', ')}` : '') +
      `\n${restamps} version(s) are re-stamps with identical code` +
      `\n${(totalBytes / 1024 / 1024).toFixed(2)} MiB of binaries -> ${out}` +
      `\nshared modules -> ${join(site, 'lib')} (${SHARED_MODULES.join(', ')})`,
  );
}

main().catch((err) => {
  console.error(`build-catalog: ${err.message}`);
  process.exit(1);
});
