import assert from 'node:assert/strict';
import { test } from 'node:test';

import { actionable, buildPlan, powershellScript, shellScript } from '../src/plan.js';
import { catalogEntry, installedEntry } from './helpers.mjs';

const catalog = (apps) => ({ schema: 1, apps });

test('an app absent from the watch is an install', () => {
  const plan = buildPlan(catalog([catalogEntry()]), []);
  assert.equal(plan.entries[0].status, 'install');
  assert.equal(actionable(plan).length, 1);
});

test('an older version on the watch is an update', () => {
  const plan = buildPlan(
    catalog([catalogEntry()]),
    [installedEntry({ version: '1.2.0', versionPacked: 0x00010200, size: 1000 })],
  );
  assert.equal(plan.entries[0].status, 'update');
});

test('the same version and size is up to date', () => {
  const plan = buildPlan(catalog([catalogEntry()]), [installedEntry()]);
  assert.equal(plan.entries[0].status, 'current');
  assert.equal(actionable(plan).length, 0);
});

test('same version but a different size is still an update', () => {
  // A truncated or half-written install reports the correct version in its
  // header, so version alone cannot be the freshness test.
  const plan = buildPlan(catalog([catalogEntry()]), [installedEntry({ size: 12345 })]);
  assert.equal(plan.entries[0].status, 'update');
});

test('a newer build on the watch is flagged, not downgraded', () => {
  const plan = buildPlan(
    catalog([catalogEntry()]),
    [installedEntry({ version: '2.0.0', versionPacked: 0x00020000 })],
  );
  assert.equal(plan.entries[0].status, 'newer-on-watch');
  assert.equal(actionable(plan).length, 0);
});

test('matches on AppID, not folder name', () => {
  // Same app, installed in a differently named folder: still an update, and the
  // existing folder is what gets reported.
  const plan = buildPlan(
    catalog([catalogEntry()]),
    [installedEntry({ folder: 'MyAlarm', version: '1.2.0', versionPacked: 0x00010200 })],
  );
  assert.equal(plan.entries[0].status, 'update');
  assert.equal(plan.entries[0].installed.folder, 'MyAlarm');
});

test('a different AppID in a same-named folder is not treated as the same app', () => {
  const plan = buildPlan(
    catalog([catalogEntry()]),
    [installedEntry({ appId: '0123456789ABCDEF' })],
  );
  assert.equal(plan.entries[0].status, 'install');
  assert.equal(plan.foreign.length, 1);
});

test('unknown apps on the watch are reported and never actioned', () => {
  const plan = buildPlan(
    catalog([catalogEntry()]),
    [installedEntry(), installedEntry({ appId: 'FFFFFFFFFFFFFFFF', folder: 'Squash' })],
  );
  assert.deepEqual(plan.foreign.map((f) => f.folder), ['Squash']);
  assert.equal(actionable(plan).length, 0);
});

test('a Glance app whose name holds a slash installs into its folder, not its name', () => {
  const app = catalogEntry({
    appId: 'A1B2C3D4E5F67890',
    name: 'AVG / R HR',
    folder: 'GlanceARHR',
    file: 'AVG_R_HR_1.3.0.uapp',
    download: 'apps/GlanceARHR/AVG_R_HR_1.3.0.uapp',
  });
  const plan = buildPlan(catalog([app]), []);
  const ps = powershellScript(plan, { baseUrl: 'https://example.test/data' });
  const sh = shellScript(plan, { baseUrl: 'https://example.test/data' });
  for (const script of [ps, sh]) {
    assert.match(script, /GlanceARHR/);
    // The display name must never reach a path.
    assert.doesNotMatch(script, /Apps.{0,4}AVG/);
  }
});

test('the PowerShell script writes the new binary before removing the stale one', () => {
  const plan = buildPlan(catalog([catalogEntry()]), []);
  const script = powershellScript(plan, { baseUrl: 'https://example.test/data' });
  const copyAt = script.indexOf('[IO.File]::Copy');
  const removeAt = script.indexOf('removed stale');
  assert.ok(copyAt > 0 && removeAt > copyAt, 'copy must precede stale removal');
  // Copy-Item has silently corrupted this volume; the .NET copy is deliberate.
  // Only invocations matter — the script explains the choice in a comment.
  assert.doesNotMatch(script, /^\s*Copy-Item\b/m);
  assert.match(script, /SHA256/);
  assert.match(script, /FileSystemLabel -eq \$Label/);
});

test('the shell script verifies the hash before copying', () => {
  const plan = buildPlan(catalog([catalogEntry()]), []);
  const script = shellScript(plan, { baseUrl: 'https://example.test/data' });
  const shaAt = script.indexOf('SHA-256 mismatch');
  const copyAt = script.indexOf('cp "$TMP');
  assert.ok(shaAt > 0 && copyAt > shaAt, 'hash check must precede the copy');
  assert.match(script, /set -eu/);
});

test('neither script touches settings.json or Activity', () => {
  const plan = buildPlan(
    catalog([catalogEntry()]),
    [installedEntry({ version: '1.2.0', versionPacked: 0x00010200 })],
  );
  for (const script of [
    powershellScript(plan, { baseUrl: 'https://x.test/data' }),
    shellScript(plan, { baseUrl: 'https://x.test/data' }),
  ]) {
    assert.doesNotMatch(script, /settings\.json['"\s]*\)?\s*(?:\||$)/m);
    // No recursive delete of an app folder anywhere.
    assert.doesNotMatch(script, /rm -rf "\$APPS/);
    assert.doesNotMatch(script, /Remove-Item -Recurse -Force \$dir/);
  }
});

test('an empty plan produces a script that still runs and does nothing', () => {
  const plan = buildPlan(catalog([catalogEntry()]), [installedEntry()]);
  const sh = shellScript(plan, { baseUrl: 'https://x.test/data' });
  assert.match(sh, /^#!\/bin\/sh/);
  assert.doesNotMatch(sh, /curl/);
});

test('single quotes in a name or folder cannot break out of the quoting', () => {
  const app = catalogEntry({ folder: "Bob's App", file: "Bob's_1.0.0.uapp" });
  const plan = buildPlan(catalog([app]), []);
  assert.match(powershellScript(plan, { baseUrl: 'x' }), /'Bob''s App'/);
  assert.match(shellScript(plan, { baseUrl: 'x' }), /'Bob'\\''s App'/);
});
