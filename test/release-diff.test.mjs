/**
 * End-to-end planner check against two real una-apps releases.
 *
 * App versions are stamped from the `apps-v*` tag and applied to every app in
 * the release, so a version bump on its own proves nothing about whether an app
 * changed. This test drives the real thing: apps-v1.2.0 as "what is installed",
 * apps-v1.3.0 as the catalogue, and asserts the planner separates genuine
 * updates from pure re-stamps.
 *
 * Opt in by pointing at two unzipped releases:
 *
 *   KIRA_FIXTURE_OLD=/path/to/apps-v1.2.0 \
 *   KIRA_FIXTURE_NEW=/path/to/apps-v1.3.0 \
 *   npm test
 */

import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';

import { parseUapp, payloadOf } from '../src/uapp.js';
import { actionable, buildPlan, describeJob } from '../src/plan.js';

const OLD = process.env.KIRA_FIXTURE_OLD;
const NEW = process.env.KIRA_FIXTURE_NEW;
const enabled = Boolean(OLD && NEW);

function sha(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

/** Walk <root>/<Folder>/<one>.uapp, mirroring the release zip layout. */
function collect(root) {
  const out = [];
  for (const folder of readdirSync(root)) {
    const dir = join(root, folder);
    if (!statSync(dir).isDirectory()) continue;
    for (const file of readdirSync(dir)) {
      if (!file.toLowerCase().endsWith('.uapp')) continue;
      const bytes = new Uint8Array(readFileSync(join(dir, file)));
      const app = parseUapp(bytes);
      out.push({ folder, file, bytes, app });
    }
  }
  return out.sort((a, b) => a.folder.localeCompare(b.folder));
}

test(
  'separates real updates from release-tag re-stamps across two releases',
  { skip: enabled ? false : 'set KIRA_FIXTURE_OLD and KIRA_FIXTURE_NEW to run' },
  () => {
    const catalog = {
      schema: 1,
      apps: collect(NEW).map(({ folder, file, bytes, app }) => ({
        appId: app.appId,
        name: app.name,
        folder,
        file,
        version: app.version,
        versionPacked: app.appVersion,
        type: app.type,
        size: bytes.length,
        sha256: sha(bytes),
        payloadSha256: sha(payloadOf(bytes)),
        download: `apps/${folder}/${file}`,
      })),
    };

    const installed = collect(OLD).map(({ folder, file, bytes, app }) => ({
      appId: app.appId,
      folder,
      file,
      name: app.name,
      version: app.version,
      versionPacked: app.appVersion,
      size: bytes.length,
      extraUapps: [],
      payloadSha256: sha(payloadOf(bytes)),
    }));

    const plan = buildPlan(catalog, installed);

    // Same 13 apps in both releases, all with a higher version in the new one.
    assert.equal(plan.foreign.length, 0, 'no unknown apps');
    assert.equal(actionable(plan).length, catalog.apps.length, 'every app is a job');
    assert.ok(
      plan.entries.every((e) => e.status === 'update'),
      'every entry should be an update, not an install',
    );

    // Sorted plainly on both sides: the planner orders by locale, which puts
    // GlanceActivity before GlanceARHR, and that ordering is not under test.
    const restamped = plan.entries
      .filter((e) => e.identicalPayload)
      .map((e) => e.app.folder)
      .sort();
    const changed = plan.entries
      .filter((e) => !e.identicalPayload)
      .map((e) => e.app.folder)
      .sort();

    // Cross-check against the bytes, computed independently of the planner, so
    // this holds for whichever two releases the fixtures point at.
    const oldByFolder = new Map(collect(OLD).map((r) => [r.folder, r]));
    const expectRestamped = [];
    const expectChanged = [];
    for (const { folder, bytes } of collect(NEW)) {
      const before = oldByFolder.get(folder);
      if (!before) continue;
      const identical = sha(payloadOf(bytes)) === sha(payloadOf(before.bytes));
      (identical ? expectRestamped : expectChanged).push(folder);
    }
    assert.deepEqual(restamped, expectRestamped.sort());
    assert.deepEqual(changed, expectChanged.sort());
    assert.ok(restamped.length + changed.length > 0, 'fixtures produced no comparisons');

    // Regression pin for the pair this behaviour was designed against: six of
    // the thirteen apps in apps-v1.3.0 are byte-identical to their 1.2.0 builds.
    const oldVersion = installed[0].version;
    const newVersion = catalog.apps[0].version;
    if (oldVersion === '1.2.0' && newVersion === '1.3.0') {
      assert.deepEqual(restamped, [
        'GlanceARHR',
        'GlanceActivity',
        'GlanceBattery',
        'GlanceFloors',
        'GlanceHR',
        'GlanceSteps',
      ].sort());
      assert.equal(changed.length, 7);
    }

    // The label must say so rather than implying new code.
    const glance = plan.entries.find((e) => e.app.folder === 'GlanceHR');
    assert.match(describeJob(glance), /identical code/);
    const alarm = plan.entries.find((e) => e.app.folder === 'Alarm');
    assert.doesNotMatch(describeJob(alarm), /identical code/);
    assert.equal(describeJob(alarm), '1.2.0 → 1.3.0');
  },
);

test('a re-stamp is still offered, because it changes what the watch reports', () => {
  const app = {
    appId: 'A1B2C3D4E5F67890',
    name: 'Live HR',
    folder: 'GlanceHR',
    file: 'Live_HR_1.3.0.uapp',
    version: '1.3.0',
    versionPacked: 0x00010300,
    size: 22980,
    sha256: 'b'.repeat(64),
    payloadSha256: 'c'.repeat(64),
    download: 'apps/GlanceHR/Live_HR_1.3.0.uapp',
  };
  const installed = {
    appId: app.appId,
    folder: 'GlanceHR',
    file: 'Live_HR_1.2.0.uapp',
    name: 'Live HR',
    version: '1.2.0',
    versionPacked: 0x00010200,
    size: 22980,
    extraUapps: [],
    payloadSha256: 'c'.repeat(64), // same code
  };

  const plan = buildPlan({ schema: 1, apps: [app] }, [installed]);
  assert.equal(plan.entries[0].status, 'update');
  assert.equal(plan.entries[0].identicalPayload, true);
  assert.equal(actionable(plan).length, 1, 'still installable');
});

test('without a hash for the installed file, an update is not claimed identical', () => {
  const app = {
    appId: 'A1B2C3D4E5F67890',
    folder: 'GlanceHR',
    file: 'x_1.3.0.uapp',
    name: 'Live HR',
    version: '1.3.0',
    versionPacked: 0x00010300,
    size: 10,
    sha256: 'b'.repeat(64),
    payloadSha256: 'c'.repeat(64),
    download: 'apps/GlanceHR/x_1.3.0.uapp',
  };
  const installed = {
    appId: app.appId,
    folder: 'GlanceHR',
    file: 'x_1.2.0.uapp',
    name: 'Live HR',
    version: '1.2.0',
    versionPacked: 0x00010200,
    size: 10,
    extraUapps: [],
    // payloadSha256 deliberately absent — not yet read off the watch.
  };

  const plan = buildPlan({ schema: 1, apps: [app] }, [installed]);
  assert.equal(plan.entries[0].identicalPayload, false);
  assert.match(describeJob(plan.entries[0]), /^1\.2\.0 → 1\.3\.0$/);
});
