import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  SCHEMA,
  describeHistory,
  findVersion,
  latestOf,
  releaseFor,
  resolveTargets,
  sortReleases,
} from '../src/catalog.js';
import { buildPlan } from '../src/plan.js';

/** Version records newest-first, as the build emits them. */
function version(v, over = {}) {
  const packed = v
    .split('.')
    .map(Number)
    .reduce((acc, n, i) => acc | (n << (16 - i * 8)), 0);
  return {
    version: v,
    versionPacked: packed >>> 0,
    tag: `apps-v${v}`,
    folder: 'GlanceHR',
    file: `Live_HR_${v}.uapp`,
    libcVersion: '0.0.3',
    autostart: false,
    size: 22980,
    sha256: `sha-${v}`,
    payloadSha256: `payload-${v}`,
    download: `apps/apps-v${v}/GlanceHR/Live_HR_${v}.uapp`,
    changed: true,
    deltaBytes: 0,
    ...over,
  };
}

function app(versions, over = {}) {
  return {
    appId: 'A1358F7C2E9D4BA6',
    name: 'Live HR',
    type: 'Glance',
    folder: 'GlanceHR',
    icon: null,
    versions,
    ...over,
  };
}

const catalog = (apps, releases = []) => ({ schema: SCHEMA, apps, releases });

test('latest is the head of the version list', () => {
  const a = app([version('1.3.0'), version('1.2.0')]);
  assert.equal(latestOf(a).version, '1.3.0');
  assert.equal(findVersion(a, '1.2.0').version, '1.2.0');
  assert.equal(findVersion(a, '9.9.9'), undefined);
});

test('unpinned apps resolve to the newest version', () => {
  const targets = resolveTargets(catalog([app([version('1.3.0'), version('1.2.0')])]));
  assert.equal(targets.length, 1);
  assert.equal(targets[0].version, '1.3.0');
  assert.equal(targets[0].isLatest, true);
  assert.equal(targets[0].download, 'apps/apps-v1.3.0/GlanceHR/Live_HR_1.3.0.uapp');
});

test('a pin selects an older version', () => {
  const a = app([version('1.3.0'), version('1.2.0')]);
  const targets = resolveTargets(catalog([a]), new Map([[a.appId, '1.2.0']]));
  assert.equal(targets[0].version, '1.2.0');
  assert.equal(targets[0].isLatest, false);
  assert.equal(targets[0].file, 'Live_HR_1.2.0.uapp');
  assert.equal(targets[0].sha256, 'sha-1.2.0');
});

test('a pin to a version that is no longer published falls back to newest', () => {
  const a = app([version('1.3.0')]);
  const targets = resolveTargets(catalog([a]), new Map([[a.appId, '0.9.0']]));
  assert.equal(targets[0].version, '1.3.0');
});

test('resolved targets are the shape the planner consumes', () => {
  // The whole point of resolveTargets: plan.js needs no knowledge of versions.
  const a = app([version('1.3.0'), version('1.2.0')]);
  const targets = resolveTargets(catalog([a]));
  const plan = buildPlan({ apps: targets }, []);
  assert.equal(plan.entries.length, 1);
  assert.equal(plan.entries[0].status, 'install');
  assert.equal(plan.entries[0].app.version, '1.3.0');
});

test('pinning an older version makes an up-to-date watch look downgraded', () => {
  const a = app([version('1.3.0'), version('1.2.0')]);
  const installed = [
    {
      appId: a.appId,
      folder: 'GlanceHR',
      file: 'Live_HR_1.3.0.uapp',
      name: 'Live HR',
      version: '1.3.0',
      versionPacked: 0x00010300,
      size: 22980,
      extraUapps: [],
    },
  ];
  const pinnedPlan = buildPlan(
    { apps: resolveTargets(catalog([a]), new Map([[a.appId, '1.2.0']])) },
    installed,
  );
  assert.equal(pinnedPlan.entries[0].status, 'newer-on-watch');
});

test('history reports which release actually changed the code', () => {
  const changed = app([
    version('1.3.0', { changed: true, deltaBytes: 17288 }),
    version('1.2.0', { changed: null, deltaBytes: null }),
  ]);
  assert.equal(describeHistory(changed), 'code changed in 1.3.0 (+17288 B)');
});

test('history reports the last release that changed an unchanged app', () => {
  const restamped = app([
    version('1.3.0', { changed: false }),
    version('1.2.0', { changed: true }),
    version('1.1.2', { changed: null }),
  ]);
  assert.equal(describeHistory(restamped), 'code unchanged since 1.2.0');
});

test('history handles an app that never changed across the whole window', () => {
  const never = app([
    version('1.3.0', { changed: false }),
    version('1.2.0', { changed: false }),
    version('1.1.2', { changed: false }),
  ]);
  assert.equal(describeHistory(never), 'code unchanged across 3 releases');
});

test('history handles a single published version', () => {
  assert.equal(describeHistory(app([version('1.3.0', { changed: null })])), 'only 1.3.0 published');
});

test('releases sort newest-first by the version in the tag', () => {
  const sorted = sortReleases([
    { tag: 'apps-v1.1.2', publishedAt: '2026-06-09T00:00:00Z' },
    { tag: 'apps-v1.3.0', publishedAt: '2026-07-22T00:00:00Z' },
    { tag: 'apps-v1.2.0', publishedAt: '2026-07-13T00:00:00Z' },
  ]);
  assert.deepEqual(sorted.map((r) => r.tag), ['apps-v1.3.0', 'apps-v1.2.0', 'apps-v1.1.2']);
});

test('tags that parse to the same version fall back to publish date', () => {
  // Upstream really does this: apps-v0.1.9-rc1/rc2/rc3 are all published as
  // full releases and all parse to 0.1.9.
  const sorted = sortReleases([
    { tag: 'apps-v0.1.9-rc1', publishedAt: '2026-05-19T00:00:00Z' },
    { tag: 'apps-v0.1.9-rc3', publishedAt: '2026-06-02T12:00:00Z' },
    { tag: 'apps-v0.1.9-rc2', publishedAt: '2026-06-02T09:00:00Z' },
  ]);
  assert.deepEqual(sorted.map((r) => r.tag), [
    'apps-v0.1.9-rc3',
    'apps-v0.1.9-rc2',
    'apps-v0.1.9-rc1',
  ]);
});

test('an unparseable tag still sorts by date without throwing', () => {
  const sorted = sortReleases([
    { tag: 'nightly', publishedAt: '2026-01-01T00:00:00Z' },
    { tag: 'apps-v1.3.0', publishedAt: '2026-07-22T00:00:00Z' },
  ]);
  assert.equal(sorted[0].tag, 'apps-v1.3.0');
});

test('release metadata is looked up by tag', () => {
  const c = catalog([], [{ tag: 'apps-v1.3.0', notes: 'hello' }]);
  assert.equal(releaseFor(c, 'apps-v1.3.0').notes, 'hello');
  assert.equal(releaseFor(c, 'apps-v9.9.9'), undefined);
  assert.equal(releaseFor({ apps: [] }, 'apps-v1.3.0'), undefined);
});
