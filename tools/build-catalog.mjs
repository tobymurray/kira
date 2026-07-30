#!/usr/bin/env node
/**
 * Build the Kira catalogue from a directory of `.uapp` files.
 *
 * Input layout is the una-apps release zip's own layout — one subdirectory per
 * app, each holding exactly one .uapp:
 *
 *   <src>/GlanceHR/Live_HR_1.3.0.uapp
 *   <src>/Alarm/Alarm_1.3.0.uapp
 *
 * The subdirectory name matters: it is the folder the watch expects under
 * Apps\ , and it is NOT derivable from the app's display name (GlanceARHR is
 * named "AVG / R HR", which contains a path separator). Everything else comes
 * out of the .uapp header itself.
 *
 * Also copies the shared ES modules from src/ into <out>/lib/ so the browser
 * runs the very same parser as this build, with no second implementation to
 * drift.
 *
 * Usage:
 *   node tools/build-catalog.mjs --src <dir> --out site \
 *        [--repo UNAWatch/una-sdk] [--tag apps-v1.3.0]
 */

import { createHash } from 'node:crypto';
import { copyFile, readdir, readFile, mkdir, writeFile, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const SHARED_MODULES = ['uapp.js', 'plan.js'];
const SRC_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', 'src');

import { parseUapp, decodeIcon, isBlankIcon, payloadOf } from '../src/uapp.js';
import { encodePng } from '../src/png.js';

function parseArgs(argv) {
  const args = { src: null, out: 'site', repo: null, tag: null };
  for (let i = 2; i < argv.length; i++) {
    const key = argv[i].replace(/^--/, '');
    if (!(key in args)) throw new Error(`unknown argument: ${argv[i]}`);
    args[key] = argv[++i];
  }
  if (!args.src) throw new Error('--src <dir> is required');
  return args;
}

/** Find <src>/<Folder>/<one>.uapp, rejecting ambiguous folders. */
async function discover(src) {
  const found = [];
  for (const entry of await readdir(src, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const dir = join(src, entry.name);
    const uapps = (await readdir(dir)).filter((f) => f.toLowerCase().endsWith('.uapp'));
    if (uapps.length === 0) continue;
    if (uapps.length > 1) {
      // The watch loads the FIRST .uapp it finds in a folder, so shipping two
      // is never right — refuse rather than pick arbitrarily.
      throw new Error(`${entry.name}: ${uapps.length} .uapp files, expected 1: ${uapps.join(', ')}`);
    }
    found.push({ folder: entry.name, file: uapps[0], path: join(dir, uapps[0]) });
  }
  return found.sort((a, b) => a.folder.localeCompare(b.folder));
}

async function main() {
  const args = parseArgs(process.argv);
  const src = resolve(args.src);
  const site = resolve(args.out);
  const out = join(site, 'data');

  const candidates = await discover(src);
  if (candidates.length === 0) {
    throw new Error(`no <Folder>/*.uapp found under ${src}`);
  }

  await rm(join(out, 'icons'), { recursive: true, force: true });
  await rm(join(out, 'apps'), { recursive: true, force: true });
  await mkdir(join(out, 'icons'), { recursive: true });

  // One parser, two runtimes: the browser imports these copies.
  await mkdir(join(site, 'lib'), { recursive: true });
  for (const name of SHARED_MODULES) {
    await copyFile(join(SRC_DIR, name), join(site, 'lib', name));
  }

  const apps = [];
  const seen = new Map();

  for (const { folder, file, path } of candidates) {
    const bytes = new Uint8Array(await readFile(path));
    const app = parseUapp(bytes);

    // A CRC failure means the kernel would silently drop this file. Never
    // publish one: the user would install it and see nothing appear.
    if (!app.crc.ok) {
      throw new Error(
        `${folder}/${file}: CRC mismatch (stored 0x${app.crc.stored?.toString(16)}, ` +
          `computed 0x${app.crc.computed?.toString(16)}) — refusing to publish`,
      );
    }

    if (seen.has(app.appId)) {
      throw new Error(
        `duplicate AppID ${app.appId}: ${folder} and ${seen.get(app.appId)} — ` +
          'the catalogue is keyed by AppID and cannot hold both',
      );
    }
    seen.set(app.appId, folder);

    // Icons: 60x60 for the grid, 30x30 kept because that is what the watch
    // shows in lists and it is free to emit.
    //
    // A declared size does not mean there is an image: Glance apps built with
    // icons off carry a zero-filled field of the full size. Emitting those as
    // PNGs would put invisible tiles in the grid, so they are skipped and the
    // UI falls back to a lettered placeholder.
    const icons = {};
    for (const [label, offset, size, suffix] of [
      ['icon', app.normalIconOffset, app.normalIconSize, ''],
      ['iconSmall', app.smallIconOffset, app.smallIconSize, '@30'],
    ]) {
      if (!size) continue;
      const raw = bytes.subarray(offset, offset + size);
      if (isBlankIcon(raw)) continue;
      const { width, height, rgba } = decodeIcon(raw);
      const rel = join('icons', `${app.appId}${suffix}.png`);
      await writeFile(join(out, rel), encodePng(rgba, width, height));
      icons[label] = rel.split('\\').join('/');
    }

    const download = ['apps', folder, file].join('/');
    await mkdir(join(out, 'apps', folder), { recursive: true });
    await writeFile(join(out, download), bytes);

    apps.push({
      appId: app.appId,
      name: app.name,
      folder,
      file,
      version: app.version,
      versionPacked: app.appVersion,
      libcVersion: app.libcVersionStr,
      type: app.type,
      autostart: app.autostart,
      size: app.size,
      serviceSize: app.serviceSize,
      guiSize: app.guiSize,
      sha256: createHash('sha256').update(bytes).digest('hex'),
      // Hash of the code alone, excluding the version stamp and CRC. Lets the
      // UI tell a real update apart from a release-tag bump.
      payloadSha256: createHash('sha256').update(payloadOf(bytes)).digest('hex'),
      ...icons,
      download,
    });

    console.log(
      `${folder.padEnd(16)} ${app.name.padEnd(14)} ${app.version.padEnd(8)} ` +
        `${app.type.padEnd(9)} ${String(app.size).padStart(7)} B  crc ok`,
    );
  }

  const catalog = {
    schema: 1,
    generated: new Date().toISOString(),
    source: { repo: args.repo, tag: args.tag },
    apps,
  };
  await writeFile(join(out, 'catalog.json'), `${JSON.stringify(catalog, null, 2)}\n`);

  const total = apps.reduce((n, a) => n + a.size, 0);
  console.log(
    `\n${apps.length} apps, ${(total / 1024 / 1024).toFixed(2)} MiB of binaries -> ${out}` +
      `\nshared modules -> ${join(site, 'lib')} (${SHARED_MODULES.join(', ')})`,
  );
}

main().catch((err) => {
  console.error(`build-catalog: ${err.message}`);
  process.exit(1);
});
