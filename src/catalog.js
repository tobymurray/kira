/**
 * Catalogue model (schema 2).
 *
 * Schema 1 held one version per app. Schema 2 holds every release Kira knows
 * about, grouped per app, newest first, because upstream publishes no per-app
 * changelog and no way to fetch a specific older build.
 *
 * `resolveTargets()` flattens a schema-2 catalogue back down to one chosen
 * version per app — the same shape the planner and script generators already
 * consume — so version selection is a concern of this module alone.
 */

import { compareVersions, parseVersion } from './uapp.js';

export const SCHEMA = 2;

/** Newest version of an app. Version lists are stored newest-first. */
export function latestOf(app) {
  return app.versions[0];
}

export function findVersion(app, version) {
  return app.versions.find((v) => v.version === version);
}

/**
 * Choose one version per app and flatten to the planner's shape.
 *
 * @param {object} catalog schema-2 catalogue
 * @param {Map<string,string>} pinned appId -> version; anything unpinned uses
 *   the newest version available.
 */
export function resolveTargets(catalog, pinned = new Map()) {
  return catalog.apps.map((app) => {
    const wanted = pinned.get(app.appId);
    // Fall back to newest if a pin refers to a version that is no longer
    // published, rather than leaving the app unresolvable.
    const version = (wanted && findVersion(app, wanted)) || latestOf(app);
    return {
      appId: app.appId,
      name: app.name,
      type: app.type,
      icon: app.icon,
      iconSmall: app.iconSmall,
      folder: version.folder,
      file: version.file,
      version: version.version,
      versionPacked: version.versionPacked,
      libcVersion: version.libcVersion,
      autostart: version.autostart,
      size: version.size,
      sha256: version.sha256,
      payloadSha256: version.payloadSha256,
      download: version.download,
      tag: version.tag,
      changed: version.changed,
      isLatest: version.version === latestOf(app).version,
    };
  });
}

/**
 * One-line history for an app, derived from bytes rather than prose.
 *
 * `changed` is computed at build time by comparing each version's payload hash
 * with the next older one, so this says whether the *code* moved, not whether
 * the release tag did.
 */
export function describeHistory(app) {
  const versions = app.versions;
  const latest = versions[0];
  if (versions.length === 1) return `only ${latest.version} published`;

  if (latest.changed === false) {
    // Walk back to the last version that actually changed the app.
    const lastReal = versions.find((v) => v.changed !== false);
    if (lastReal && lastReal !== latest) {
      return `code unchanged since ${lastReal.version}`;
    }
    return `code unchanged across ${versions.length} releases`;
  }

  const delta = latest.deltaBytes;
  const size = typeof delta === 'number' && delta !== 0
    ? ` (${delta > 0 ? '+' : ''}${delta} B)`
    : '';
  return `code changed in ${latest.version}${size}`;
}

/** Release metadata for a tag, if the build captured any. */
export function releaseFor(catalog, tag) {
  return (catalog.releases ?? []).find((r) => r.tag === tag);
}

/**
 * Sort release descriptors newest-first.
 *
 * Prefers the version embedded in the tag, since that is what the binaries are
 * stamped with; falls back to publish date for tags that do not parse.
 */
export function sortReleases(releases) {
  return [...releases].sort((a, b) => {
    const av = parseVersion(String(a.tag).replace(/^apps-/, ''));
    const bv = parseVersion(String(b.tag).replace(/^apps-/, ''));
    if (av !== null && bv !== null && av !== bv) return compareVersions(bv, av);
    return String(b.publishedAt ?? '').localeCompare(String(a.publishedAt ?? ''));
  });
}
