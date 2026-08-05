//! The published catalogue, and selection of a version per app.
//!
//! Schema 1 held one version per app. Schema 2 holds every release Kira knows
//! about, grouped per app, newest first, because upstream publishes no per-app
//! changelog and no way to fetch a specific older build.
//!
//! [`resolve_targets`] flattens a catalogue down to one chosen version per app —
//! the shape [`crate::plan`] consumes — so version selection lives here alone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::prerelease::{self, PreRelease, Precedence};
pub use crate::uapp::AppType;
use crate::uapp::{AppId, Version};

/// Schema version emitted and expected by this crate.
///
/// 3 adds provenance: who built each binary, by what recipe, and how it relates
/// to what upstream published. 4 adds submissions: who publishes an app that is
/// not upstream's, and why a listing or a single version is no longer offered.
/// 5 lets a submitted version carry its own note, since the upstream release
/// bodies say nothing about an app that does not ship in a release. 6 lets an
/// app declare a settings file it reads from its own folder, so the page can
/// fill it in over USB — the one route a user-specific value has onto a watch
/// with four buttons and no keyboard. 7 publishes upstream's release candidates,
/// which needs two fields a version number cannot carry: which stage a build is
/// (`prerelease`, since the binary stamps `1.4.0` for both `apps-v1.4.0-rc1` and
/// `apps-v1.4.0`), and the hashes of the candidates a release supersedes, so a
/// watch carrying one is offered the release rather than reported as a stranger.
pub const SCHEMA: u32 = 7;

/// A complete catalogue, as published to `data/catalog.json`.
///
/// Field order is the serialised order; it matches schema 2 exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// Always [`SCHEMA`].
    pub schema: u32,
    /// RFC 3339 build timestamp.
    pub generated: String,
    /// Where the binaries came from.
    pub source: Source,
    /// Releases included, newest first.
    pub releases: Vec<Release>,
    /// Apps, sorted by display name.
    pub apps: Vec<App>,
}

/// Provenance of the published binaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Upstream repository, e.g. `UNAWatch/una-sdk`.
    pub repo: Option<String>,
}

/// One upstream release.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    /// Git tag, e.g. `apps-v1.3.0`.
    pub tag: String,
    /// RFC 3339 publish time, if known.
    pub published_at: Option<String>,
    /// Link to the upstream release.
    pub url: Option<String>,
    /// Whether upstream marked it a pre-release.
    pub is_prerelease: bool,
    /// The release body, verbatim.
    ///
    /// Third-party Markdown. Render as text, never as HTML.
    pub notes: Option<String>,
    /// How many apps this release contributed.
    pub app_count: usize,
}

/// An app, with every version of it that Kira publishes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    /// Stable identity. Two entries may share a display name while differing
    /// here: upstream reassigned the IDs of three Glances after `apps-v0.1.9-rc1`.
    pub app_id: AppId,
    /// Display name from the newest build.
    pub name: String,
    /// What kind of app this is.
    #[serde(rename = "type")]
    pub app_type: AppType,
    /// On-device folder under `Apps\`, from the newest build.
    pub folder: String,
    /// Versions, newest first.
    pub versions: Vec<VersionEntry>,
    /// Path to the 60x60 icon, absent when no version carries pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Path to the 30x30 icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_small: Option<String>,
    /// Another app that occupies the same on-device folder with newer versions.
    ///
    /// Two apps cannot share `Apps/<Folder>/`: the watch loads whichever `.uapp`
    /// it finds first, so installing both would risk booting the wrong one. This
    /// happens because upstream reassigned the ids of three Glances, leaving the
    /// old identity behind with only ancient versions. Such an app stays in the
    /// catalogue -- its binaries are still downloadable -- but is never offered
    /// for installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<AppId>,
    /// Who publishes this app, when it is not upstream's.
    ///
    /// Absent for an app that ships in the SDK, whose publisher is the
    /// catalogue's own [`Source`]. Its presence is what distinguishes a
    /// submission, and it is deliberately not a rank: see `registry/README.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    /// A settings file this app reads from `Apps/<Folder>/`, if it has one.
    ///
    /// The only thing on a card that cannot be derived from the binary: nothing
    /// in a `.uapp` says what it reads, so this is the submitter's assertion.
    /// It is also the only assertion Kira acts on rather than merely renders —
    /// it decides a file name written to a device — which is why
    /// [`crate::config::check_spec`] is stricter than the shape needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<crate::config::Spec>,
    /// Why the whole app is no longer offered, if it is not.
    ///
    /// A retired app stays listed and keeps its binaries, so a watch carrying it
    /// is recognised and its owner told why -- more use than reporting it as
    /// something unknown. The reason is the point; a bare flag would say nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<String>,
}

/// Who publishes an app that did not come from the SDK.
///
/// Kira still builds the binary itself, from the commit the manifest pins, so
/// this names the source rather than the builder. What each version was built
/// from is in its [`BuiltFrom::app_source`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Publisher {
    /// Repository the source is taken from.
    pub repo: String,
    /// Handle to reach about the submission.
    pub maintainer: String,
}

/// Who produced the binary Kira serves for a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// Republished verbatim from an upstream release.
    Upstream,
    /// Built by Kira from source.
    Kira,
}

/// Everything needed to rebuild a binary and get the same bytes.
///
/// Published so the claim "built from this source" can be checked rather than
/// taken on trust. See `docs/reproducibility.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltFrom {
    /// Canonical identity of the app's source.
    pub app_source: String,
    /// SDK revision it was compiled against.
    pub sdk_rev: String,
    /// Toolchain container, by digest.
    pub toolchain: String,
    /// Hash of the whole recipe, for cache identity.
    pub recipe: String,
}

/// One published build of an app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    /// Version as stamped from the release tag.
    pub version: Version,
    /// The same value packed, retained for schema compatibility.
    pub version_packed: u32,
    /// Which stage of [`Self::version`] this build is, when it is not the release.
    ///
    /// `rc1` for `apps-v1.4.0-rc1`. Absent for a full release, which is why it is
    /// absent from almost every entry.
    ///
    /// It has to be stored rather than derived from [`Self::version`], because the
    /// packer stamps the version it parses out of the tag: a candidate's binary
    /// reports itself as `1.4.0` exactly as the release does. The tag is the only
    /// place the difference survives. See [`crate::prerelease`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prerelease: Option<PreRelease>,
    /// Release this build came from.
    pub tag: String,
    /// On-device folder for this build.
    pub folder: String,
    /// File name, which encodes the version.
    pub file: String,
    /// `LibC` ABI this build was linked against.
    pub libc_version: Version,
    /// Whether it starts at boot.
    pub autostart: bool,
    /// File length in bytes.
    pub size: usize,
    /// Hash of the whole file.
    pub sha256: String,
    /// Hash of the code alone, excluding the version stamp and CRC footer.
    pub payload_sha256: String,
    /// Path under `data/` to download this build.
    pub download: String,
    /// Whether the code differs from the next older version. `None` when there
    /// is no older version published here, which is unknown rather than false.
    pub changed: Option<bool>,
    /// Size difference against the next older version.
    pub delta_bytes: Option<i64>,
    /// Who produced the binary being served.
    pub origin: Origin,
    /// How to reproduce it, when Kira built it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_from: Option<BuiltFrom>,
    /// Hash of the binary upstream published for this same app and version.
    ///
    /// Recorded even when Kira serves its own build, so a watch carrying the
    /// vendor's binary can be recognised rather than nagged about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_sha256: Option<String>,
    /// Whether the served binary is byte-identical to upstream's.
    ///
    /// Derived, not asserted. Before the SDK carries the path-independence fix
    /// this is expected to be false for Kira-built binaries; it should start
    /// coming out true on its own afterwards, with no special-casing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matches_upstream: Option<bool>,
    /// Hashes of builds this entry displaced at the *same* version number.
    ///
    /// The release candidates Kira used to publish for this version, and no longer
    /// does: only the newest candidate is listed at a time, and a full release
    /// displaces it entirely, so the entries themselves are gone. Their hashes stay
    /// because they are the only way to recognise a watch still carrying one — a
    /// candidate stamps the version it is a candidate for, so `1.4.0-rc1` and
    /// `1.4.0` are indistinguishable by version and tell apart only by hash.
    ///
    /// Without this the catalogue would forget, and whoever took a candidate would
    /// be told they had an unrecognised build of the current version: reported,
    /// never offered, stuck until they deleted it by hand.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes_sha256: Vec<String>,
    /// Why this particular version is no longer offered, if it is not.
    ///
    /// Independent of the app's own [`App::retired`]: a submitter can withdraw
    /// one bad build without taking the listing down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<String>,
    /// What this version changed, in the publisher's own words.
    ///
    /// Only submissions carry one. An SDK app is described by the release body
    /// of the tag it shipped in, which covers the whole repository; a submission
    /// ships on its own and no release body mentions it at all.
    ///
    /// An assertion, not something derived — which is why it sits beside
    /// [`Self::changed`] rather than replacing it. The one says what the author
    /// meant to change; the other says whether the code moved. Third-party
    /// prose: render as text, never as HTML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// One version of one app, chosen for installation or download.
///
/// Deliberately flat: the planner should not have to know that versions exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    /// Stable identity.
    pub app_id: AppId,
    /// Display name.
    pub name: String,
    /// What kind of app this is.
    #[serde(rename = "type")]
    pub app_type: AppType,
    /// Path to the 60x60 icon, if any.
    pub icon: Option<String>,
    /// Path to the 30x30 icon, if any.
    pub icon_small: Option<String>,
    /// On-device folder under `Apps\`.
    pub folder: String,
    /// File name to write.
    pub file: String,
    /// Chosen version.
    pub version: Version,
    /// Which stage of [`Self::version`] this is, when it is not the release.
    pub prerelease: Option<PreRelease>,
    /// Hashes of published builds this one supersedes at the *same* version.
    ///
    /// Only ever the candidates a release replaces. It exists because nothing else
    /// can tell them apart: `apps-v1.4.0-rc1` and `apps-v1.4.0` both stamp `1.4.0`,
    /// so a watch carrying the candidate reports the same version as the release
    /// and the planner would call it an unrecognised build of the current version —
    /// reported, never offered, leaving whoever took the candidate stranded on it.
    /// Matching the installed hash against this list is what makes the release an
    /// update. See [`crate::plan::Recognised::CandidateBuild`].
    pub supersedes_sha256: Vec<String>,
    /// `LibC` ABI this build needs.
    pub libc_version: Version,
    /// Whether it starts at boot.
    pub autostart: bool,
    /// File length in bytes.
    pub size: usize,
    /// Hash of the whole file, checked before writing.
    pub sha256: String,
    /// Hash of the code alone.
    pub payload_sha256: String,
    /// Path under `data/` to fetch it from.
    pub download: String,
    /// Release it came from.
    pub tag: String,
    /// Whether this version changed the code.
    pub changed: Option<bool>,
    /// Whether this is the newest published version.
    pub is_latest: bool,
    /// Set when another app owns this on-device folder with newer versions.
    pub superseded_by: Option<AppId>,
    /// Who produced this binary.
    pub origin: Origin,
    /// How to reproduce it, when Kira built it.
    pub built_from: Option<BuiltFrom>,
    /// Hash of upstream's binary for the same version, if known.
    pub upstream_sha256: Option<String>,
    /// Whether the two are byte-identical.
    pub matches_upstream: Option<bool>,
    /// Why this is not on offer, if it is not: the app was withdrawn, or this
    /// version was.
    pub retired: Option<String>,
}

impl VersionEntry {
    /// This build's ordering key: version, then stage within it.
    #[must_use]
    pub fn precedence(&self) -> Precedence {
        prerelease::precedence(self.version, self.prerelease.as_ref())
    }

    /// How this build is named, and how a selection refers to it.
    ///
    /// `1.4.0` or `1.4.0-rc1`. Unique within an app, which [`Self::version`] is not
    /// once candidates are published: two entries would both read `1.4.0` and a
    /// version picker could not tell a reader which was which.
    #[must_use]
    pub fn label(&self) -> String {
        prerelease::label(self.version, self.prerelease.as_ref())
    }

    /// Whether this build is a release candidate rather than a full release.
    #[must_use]
    pub fn is_prerelease(&self) -> bool {
        self.prerelease.is_some()
    }
}

impl Target {
    /// How this build is named to a reader: `1.4.0` or `1.4.0-rc1`.
    ///
    /// Anything naming a build *Kira publishes* uses this. The bare
    /// [`Self::version`] is right only where the subject is a build read off a
    /// watch, whose header cannot distinguish a candidate from the release it
    /// became -- so claiming a stage there would be inventing one.
    #[must_use]
    pub fn label(&self) -> String {
        prerelease::label(self.version, self.prerelease.as_ref())
    }
}

impl App {
    /// The build offered by default: the newest full release, if there is one.
    ///
    /// **Not simply the head of the list.** Version lists are stored highest
    /// precedence first, and a release candidate outranks every earlier release —
    /// so taking the head would move every app in the catalogue onto a candidate
    /// the moment upstream tagged one, which is a stability regression nobody asked
    /// for. A candidate is the default only for an app that has no full release at
    /// all, which is exactly the case it exists to serve: `Stopwatch` first shipped
    /// in `apps-v1.4.0-rc1` and is otherwise unreachable.
    ///
    /// Every candidate stays selectable in the version picker either way.
    ///
    /// # Panics
    /// If the app has no versions, which the builder never emits.
    #[must_use]
    pub fn latest(&self) -> &VersionEntry {
        let newest = || {
            self.versions
                .first()
                .expect("catalogue apps always have at least one version")
        };
        // The list is precedence-ordered, so the first full release in it is the
        // newest one.
        self.versions
            .iter()
            .find(|v| !v.is_prerelease())
            .unwrap_or_else(newest)
    }

    /// Look up a specific build by its [`VersionEntry::label`].
    ///
    /// By label rather than by version, because a version stopped being unique the
    /// moment candidates were published.
    #[must_use]
    pub fn find(&self, label: &str) -> Option<&VersionEntry> {
        self.versions.iter().find(|v| v.label() == label)
    }

    /// A one-line history, derived from bytes rather than prose.
    ///
    /// `changed` is computed at build time by comparing payload hashes, so this
    /// says whether the *code* moved, not whether the release tag did.
    #[must_use]
    /// Every build here is named by [`VersionEntry::label`] rather than by its
    /// version. "only 1.4.0 published" for an app whose one build is
    /// `apps-v1.4.0-rc1` names a release that does not exist yet, which is the
    /// opposite of what this line is for.
    pub fn describe_history(&self) -> String {
        let latest = self.latest();
        if self.versions.len() == 1 {
            return format!("only {} published", latest.label());
        }

        if latest.changed == Some(false) {
            // Walk back to the last version that actually changed the app.
            let last_real = self.versions.iter().find(|v| v.changed != Some(false));
            return match last_real {
                Some(v) if v.label() != latest.label() => {
                    format!("code unchanged since {}", v.label())
                }
                _ => format!("code unchanged across {} releases", self.versions.len()),
            };
        }

        if latest.changed.is_none() {
            // Not comparable: the two versions were produced by different
            // builders, so a byte difference says nothing about the code. Saying
            // "changed" here would be the false claim the build deliberately
            // avoided recording.
            return match self.versions.get(1) {
                Some(older) => format!("not comparable with {}", older.label()),
                None => format!("only {} published", latest.label()),
            };
        }

        match latest.delta_bytes {
            Some(delta) if delta != 0 => {
                format!("code changed in {} ({delta:+} B)", latest.label())
            }
            _ => format!("code changed in {}", latest.label()),
        }
    }
}

impl Catalog {
    /// Release metadata for a tag.
    #[must_use]
    pub fn release(&self, tag: &str) -> Option<&Release> {
        self.releases.iter().find(|r| r.tag == tag)
    }

    /// Display names shared by more than one [`AppId`], so a UI can say which is
    /// which instead of showing identical-looking entries.
    ///
    /// Superseded and retired apps are excluded: both belong in an archive of
    /// their own, so their name no longer collides with anything a reader is
    /// choosing between.
    #[must_use]
    pub fn ambiguous_names(&self) -> Vec<String> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for app in self
            .apps
            .iter()
            .filter(|a| a.superseded_by.is_none() && a.retired.is_none())
        {
            *counts.entry(app.name.as_str()).or_default() += 1;
        }
        counts
            .into_iter()
            .filter(|&(_, n)| n > 1)
            .map(|(name, _)| name.to_owned())
            .collect()
    }
}

/// What a release did to the apps in it, decided by comparing binaries.
///
/// The prose in a release body describes the whole repository; this describes the
/// watch. `unknown` counts apps whose build is not comparable with the previous
/// one — different builders produced them, so a byte difference says nothing —
/// which is honestly distinct from "unchanged".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseEffect {
    /// Display names of apps whose code changed, in catalogue order.
    pub changed: Vec<String>,
    /// How many apps in the release carried the same code as before.
    pub unchanged: usize,
    /// How many could not be compared.
    pub unknown: usize,
    /// How many apps had no earlier release to compare against.
    pub first_seen: usize,
}

/// Summarise one release's effect on the apps.
#[must_use]
pub fn changed_in(apps: &[App], tag: &str) -> ReleaseEffect {
    let mut effect = ReleaseEffect::default();
    for app in apps {
        // Versions are newest first, so the position gives the older neighbour.
        let Some(index) = app.versions.iter().position(|v| v.tag == tag) else {
            continue;
        };
        let version = &app.versions[index];
        if app.versions.len() == index + 1 {
            effect.first_seen += 1;
            continue;
        }
        match version.changed {
            Some(true) => effect.changed.push(app.name.clone()),
            Some(false) => effect.unchanged += 1,
            None => effect.unknown += 1,
        }
    }
    effect
}

/// Mark apps that lose a fight over an on-device folder.
///
/// Within a folder the app with the newest version wins; the rest are superseded
/// and must not be offered for installation, since writing them would put a
/// second `.uapp` in a folder the watch resolves by taking the first it finds.
pub fn mark_superseded(apps: &mut [App]) {
    let mut best: BTreeMap<String, (Precedence, AppId)> = BTreeMap::new();
    for app in apps.iter() {
        let newest = app.latest().precedence();
        let folder = app.folder.clone();
        // Ties broken by id so the outcome does not depend on iteration order.
        let candidate = (newest, app.app_id);
        best.entry(folder)
            .and_modify(|current| {
                if candidate > *current {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    }

    for app in apps {
        if let Some(&(_, winner)) = best.get(&app.folder) {
            app.superseded_by = (winner != app.app_id).then_some(winner);
        }
    }
}

/// Choose one version per app and flatten to the planner's shape.
///
/// `pinned` maps an app to a specific version; anything unpinned, or pinned to a
/// version that is no longer published, uses the newest available rather than
/// becoming unresolvable.
#[must_use]
pub fn resolve_targets(catalog: &Catalog, pinned: &BTreeMap<AppId, String>) -> Vec<Target> {
    catalog
        .apps
        .iter()
        .map(|app| {
            let chosen = pinned
                .get(&app.app_id)
                .and_then(|label| app.find(label))
                .unwrap_or_else(|| app.latest());

            Target {
                app_id: app.app_id,
                name: app.name.clone(),
                app_type: app.app_type,
                icon: app.icon.clone(),
                icon_small: app.icon_small.clone(),
                folder: chosen.folder.clone(),
                file: chosen.file.clone(),
                version: chosen.version,
                prerelease: chosen.prerelease.clone(),
                supersedes_sha256: chosen.supersedes_sha256.clone(),
                libc_version: chosen.libc_version,
                autostart: chosen.autostart,
                size: chosen.size,
                sha256: chosen.sha256.clone(),
                payload_sha256: chosen.payload_sha256.clone(),
                download: chosen.download.clone(),
                tag: chosen.tag.clone(),
                changed: chosen.changed,
                is_latest: chosen.precedence() == app.latest().precedence(),
                superseded_by: app.superseded_by,
                origin: chosen.origin,
                built_from: chosen.built_from.clone(),
                upstream_sha256: chosen.upstream_sha256.clone(),
                matches_upstream: chosen.matches_upstream,
                // A withdrawn app withdraws every version of itself; a single
                // version can also be withdrawn on its own.
                retired: app.retired.clone().or_else(|| chosen.retired.clone()),
            }
        })
        .collect()
}

/// A submission's source, as recorded in a recipe's `app_source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRef {
    /// Repository the source was fetched from.
    pub repo: String,
    /// The exact commit built.
    pub rev: String,
    /// Path within the repository to the app root.
    pub subdir: String,
}

/// Read back the `git:<url>@<sha>:<subdir>` form a submission's recipe records.
///
/// Lives here so the page and the build agree on what a recipe means rather than
/// each picking the string apart. `None` for anything else, notably the
/// `sdk:<tag>:<path>` form: an SDK app has no separate repository to name.
#[must_use]
pub fn parse_source(app_source: &str) -> Option<SourceRef> {
    // The URL cannot itself contain '@' -- a submission carrying credentials is
    // refused -- so the first one separates it from the revision.
    let (repo, rest) = app_source.strip_prefix("git:")?.split_once('@')?;
    let (rev, subdir) = rest.split_once(':')?;
    if repo.is_empty() || rev.is_empty() {
        return None;
    }
    Some(SourceRef {
        repo: repo.to_owned(),
        rev: rev.to_owned(),
        subdir: subdir.to_owned(),
    })
}

/// The version embedded in a release tag, e.g. `apps-v1.3.0` or `apps-v0.1.9-rc3`.
///
/// Parses from the first digit, so any prefix convention works.
#[must_use]
pub fn version_from_tag(tag: &str) -> Option<Version> {
    let start = tag.find(|c: char| c.is_ascii_digit())?;
    tag[start..].parse().ok()
}

/// Anything that can be ordered as a release.
pub trait ReleaseOrder {
    /// The git tag.
    fn tag(&self) -> &str;
    /// Publish time, if known.
    fn published_at(&self) -> Option<&str>;
}

impl ReleaseOrder for Release {
    fn tag(&self) -> &str {
        &self.tag
    }

    fn published_at(&self) -> Option<&str> {
        self.published_at.as_deref()
    }
}

/// Sort releases newest first.
///
/// Prefers the version in the tag, since that is what the binaries are stamped
/// with, and falls back to publish date. The fallback is load-bearing: upstream
/// publishes `apps-v0.1.9-rc1`, `-rc2` and `-rc3` as full releases and all three
/// parse to `0.1.9`.
pub fn sort_newest_first<T: ReleaseOrder>(releases: &mut [T]) {
    releases.sort_by(|a, b| {
        let av = version_from_tag(a.tag());
        let bv = version_from_tag(b.tag());
        bv.cmp(&av).then_with(|| {
            b.published_at()
                .unwrap_or_default()
                .cmp(a.published_at().unwrap_or_default())
        })
    });
}

/// Split entries into those with a unique id, and a description of collisions.
///
/// [`AppId`] is the identity everything keys on, so two apps in one release
/// claiming the same id makes both unattributable — `apps-v0.1.9-rc3` really does
/// ship `GlanceStrain` and `GlanceActivity` under `A1E84D2F7A9C5B60`. Every side
/// of a collision is dropped rather than guessed at, because the wrong guess
/// installs the wrong binary.
pub fn partition_unique<T, I, L>(entries: Vec<T>, id_of: I, label_of: L) -> Partitioned<T>
where
    I: Fn(&T) -> AppId,
    L: Fn(&T) -> String,
{
    let mut groups: BTreeMap<AppId, Vec<T>> = BTreeMap::new();
    for entry in entries {
        groups.entry(id_of(&entry)).or_default().push(entry);
    }

    let mut unique = Vec::new();
    let mut collisions = Vec::new();
    for (id, group) in groups {
        // TryFrom gives the single-element case without an unwrap, handing the
        // Vec back untouched when there is more than one.
        match <[T; 1]>::try_from(group) {
            Ok([only]) => unique.push(only),
            Err(group) => collisions.push(Collision {
                app_id: id,
                labels: group.iter().map(&label_of).collect(),
            }),
        }
    }
    Partitioned { unique, collisions }
}

/// Output of [`partition_unique`].
#[derive(Debug)]
pub struct Partitioned<T> {
    /// Entries whose id appeared exactly once.
    pub unique: Vec<T>,
    /// Ids claimed more than once, with the labels that claimed them.
    pub collisions: Vec<Collision>,
}

/// Two or more entries claiming one [`AppId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    /// The contested identity.
    pub app_id: AppId,
    /// Labels of everything that claimed it, e.g. folder names.
    pub labels: Vec<String>,
}

impl std::fmt::Display for Collision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} claimed by {}",
            self.app_id,
            self.labels.join(" and ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version_entry(v: &str) -> VersionEntry {
        let version: Version = v.parse().unwrap();
        VersionEntry {
            version,
            version_packed: version.packed(),
            prerelease: None,
            supersedes_sha256: Vec::new(),
            tag: format!("apps-v{v}"),
            folder: "GlanceHR".into(),
            file: format!("Live_HR_{v}.uapp"),
            libc_version: Version::new(0, 0, 3),
            autostart: false,
            size: 22980,
            sha256: format!("sha-{v}"),
            payload_sha256: format!("payload-{v}"),
            download: format!("apps/apps-v{v}/GlanceHR/Live_HR_{v}.uapp"),
            changed: Some(true),
            delta_bytes: Some(0),
            origin: Origin::Kira,
            built_from: None,
            upstream_sha256: None,
            matches_upstream: None,
            retired: None,
            notes: None,
        }
    }

    fn app(versions: Vec<VersionEntry>) -> App {
        App {
            app_id: AppId::new(0xA135_8F7C_2E9D_4BA6),
            name: "Live HR".into(),
            app_type: AppType::Glance,
            folder: "GlanceHR".into(),
            versions,
            icon: None,
            icon_small: None,
            superseded_by: None,
            publisher: None,
            config: None,
            retired: None,
        }
    }

    #[test]
    fn a_releases_effect_on_the_apps_comes_from_the_binaries() {
        let changed = version_entry("1.3.0");
        let mut same = version_entry("1.3.0");
        same.changed = Some(false);
        let mut incomparable = version_entry("1.3.0");
        incomparable.changed = None;

        let mut second = app(vec![same, version_entry("1.2.0")]);
        second.name = "Steps".into();
        let mut third = app(vec![incomparable, version_entry("1.2.0")]);
        third.name = "Floors".into();
        // Only one release published, so there is nothing to compare against.
        let mut fourth = app(vec![version_entry("1.3.0")]);
        fourth.name = "New App".into();

        let apps = vec![
            app(vec![changed, version_entry("1.2.0")]),
            second,
            third,
            fourth,
        ];

        let effect = changed_in(&apps, "apps-v1.3.0");
        assert_eq!(effect.changed, ["Live HR"]);
        assert_eq!(effect.unchanged, 1);
        assert_eq!(effect.unknown, 1);
        assert_eq!(effect.first_seen, 1);

        // An app absent from a release is not counted at all.
        let older = changed_in(&apps, "apps-v1.2.0");
        assert!(older.changed.is_empty());
        assert_eq!(older.first_seen, 3, "1.2.0 is the oldest published here");
        assert_eq!(changed_in(&apps, "apps-v9.9.9"), ReleaseEffect::default());
    }

    fn catalog(apps: Vec<App>) -> Catalog {
        Catalog {
            schema: SCHEMA,
            generated: "2026-07-30T00:00:00.000Z".into(),
            source: Source { repo: None },
            releases: Vec::new(),
            apps,
        }
    }

    /// A candidate and the release it became, as the catalogue holds them: same
    /// version number, different builds, the release first.
    fn app_with_candidate() -> App {
        let mut release = version_entry("1.4.0");
        release.sha256 = "release-bytes".into();
        let mut candidate = version_entry("1.4.0");
        candidate.prerelease = PreRelease::for_release("apps-v1.4.0-rc1", true);
        candidate.tag = "apps-v1.4.0-rc1".into();
        candidate.sha256 = "candidate-bytes".into();
        app(vec![release, candidate, version_entry("1.3.0")])
    }

    #[test]
    fn a_release_outranks_its_own_candidate_at_the_same_version() {
        let a = app_with_candidate();
        assert_eq!(a.latest().label(), "1.4.0");
        assert!(!a.latest().is_prerelease(), "the release must win the tie");
        assert!(a.versions[1].is_prerelease());
    }

    #[test]
    fn a_candidate_does_not_become_the_default_for_an_app_that_has_a_release() {
        // Upstream tagging apps-v1.4.0-rc1 must not move every settled app in the
        // catalogue onto a candidate. The candidate outranks 1.3.0 and still sits
        // at the head of the list -- it is selectable -- but 1.3.0 is what is
        // offered.
        let mut candidate = version_entry("1.4.0");
        candidate.prerelease = PreRelease::for_release("apps-v1.4.0-rc1", true);
        let settled = app(vec![candidate, version_entry("1.3.0")]);

        assert!(settled.versions[0].is_prerelease(), "still ranked first");
        assert_eq!(settled.latest().label(), "1.3.0", "but not offered");
    }

    #[test]
    fn a_candidate_is_the_default_when_it_is_the_only_build_there_is() {
        // Stopwatch's case, and the reason candidates are published at all: it
        // shipped for the first time in apps-v1.4.0-rc1.
        let mut only = version_entry("1.4.0");
        only.prerelease = PreRelease::for_release("apps-v1.4.0-rc1", true);
        let fresh = app(vec![only]);
        assert_eq!(fresh.latest().label(), "1.4.0-rc1");
    }

    #[test]
    fn a_build_is_found_by_label_since_versions_no_longer_identify_one() {
        let a = app_with_candidate();
        assert_eq!(
            a.find("1.4.0").map(|v| v.sha256.clone()),
            Some("release-bytes".into())
        );
        assert_eq!(
            a.find("1.4.0-rc1").map(|v| v.sha256.clone()),
            Some("candidate-bytes".into())
        );
        assert!(a.find("1.4.0-rc2").is_none());
    }

    #[test]
    fn a_chosen_build_carries_its_recorded_supersedes_list() {
        // Recorded at build time by `collapse_candidates`, since the entries it
        // names are deliberately no longer in the catalogue. resolve_targets only
        // has to carry it through to the planner intact.
        let mut a = app_with_candidate();
        a.versions.retain(|v| !v.is_prerelease());
        a.versions[0].supersedes_sha256 = vec!["candidate-bytes".into()];

        let c = catalog(vec![a]);
        let targets = resolve_targets(&c, &BTreeMap::new());
        assert_eq!(targets[0].version, Version::new(1, 4, 0));
        assert_eq!(targets[0].prerelease, None);
        assert_eq!(targets[0].supersedes_sha256, ["candidate-bytes"]);
    }

    #[test]
    fn a_candidate_supersedes_nothing_and_says_so() {
        // Pinned to a candidate that displaced nothing: overwriting a build on the
        // strength of an empty list must not be possible.
        let c = catalog(vec![app_with_candidate()]);
        let pinned = BTreeMap::from([(c.apps[0].app_id, "1.4.0-rc1".to_owned())]);
        let targets = resolve_targets(&c, &pinned);
        assert_eq!(
            targets[0].prerelease.as_ref().map(PreRelease::as_str),
            Some("rc1")
        );
        assert!(targets[0].supersedes_sha256.is_empty());
        assert!(!targets[0].is_latest, "the release is the latest, not this");
    }

    #[test]
    fn history_never_names_a_release_that_is_not_published() {
        // The card said "only 1.4.0 published" for an app whose only build was
        // apps-v1.4.0-rc1 -- naming the release as published when the candidate is
        // the whole reason the app is listed at all.
        let mut only = version_entry("1.4.0");
        only.prerelease = PreRelease::for_release("apps-v1.4.0-rc1", true);
        let fresh = app(vec![only]);
        assert_eq!(fresh.describe_history(), "only 1.4.0-rc1 published");

        // And in the other arms: whichever build is named, it is named as
        // published, candidate or not.
        let mut newest = version_entry("1.4.0");
        newest.prerelease = PreRelease::for_release("apps-v1.4.0-rc1", true);
        newest.changed = None;
        let two = app(vec![newest, version_entry("1.3.0")]);
        assert_eq!(two.describe_history(), "code changed in 1.3.0");
    }

    #[test]
    fn history_names_a_candidate_when_the_candidate_is_what_moved() {
        // No full release at all, so the candidate is both the default and the
        // subject of the history line.
        let mut newer = version_entry("1.4.0");
        newer.prerelease = PreRelease::for_release("apps-v1.4.0-rc2", true);
        newer.delta_bytes = Some(512);
        let mut older = version_entry("1.4.0");
        older.prerelease = PreRelease::for_release("apps-v1.4.0-rc1", true);
        older.changed = None;

        let a = app(vec![newer, older]);
        assert_eq!(a.describe_history(), "code changed in 1.4.0-rc2 (+512 B)");
    }

    #[test]
    fn latest_is_the_head_of_the_list() {
        let a = app(vec![version_entry("1.3.0"), version_entry("1.2.0")]);
        assert_eq!(a.latest().version, Version::new(1, 3, 0));
        assert!(a.find("1.2.0").is_some());
        assert!(a.find("9.9.9").is_none());
    }

    #[test]
    fn unpinned_apps_resolve_to_the_newest_version() {
        let c = catalog(vec![app(vec![
            version_entry("1.3.0"),
            version_entry("1.2.0"),
        ])]);
        let targets = resolve_targets(&c, &BTreeMap::new());
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].version, Version::new(1, 3, 0));
        assert!(targets[0].is_latest);
    }

    #[test]
    fn a_pin_selects_an_older_version() {
        let c = catalog(vec![app(vec![
            version_entry("1.3.0"),
            version_entry("1.2.0"),
        ])]);
        let pinned = BTreeMap::from([(c.apps[0].app_id, "1.2.0".to_owned())]);
        let targets = resolve_targets(&c, &pinned);
        assert_eq!(targets[0].version, Version::new(1, 2, 0));
        assert!(!targets[0].is_latest);
        assert_eq!(targets[0].file, "Live_HR_1.2.0.uapp");
    }

    #[test]
    fn a_pin_to_an_unpublished_version_falls_back_to_newest() {
        let c = catalog(vec![app(vec![version_entry("1.3.0")])]);
        let pinned = BTreeMap::from([(c.apps[0].app_id, "0.9.0".to_owned())]);
        assert_eq!(
            resolve_targets(&c, &pinned)[0].version,
            Version::new(1, 3, 0)
        );
    }

    #[test]
    fn history_reports_which_release_changed_the_code() {
        let mut newest = version_entry("1.3.0");
        newest.delta_bytes = Some(17288);
        let mut oldest = version_entry("1.2.0");
        oldest.changed = None;
        oldest.delta_bytes = None;
        assert_eq!(
            app(vec![newest, oldest]).describe_history(),
            "code changed in 1.3.0 (+17288 B)"
        );
    }

    #[test]
    fn history_reports_the_last_release_that_changed_an_unchanged_app() {
        let mut newest = version_entry("1.3.0");
        newest.changed = Some(false);
        let middle = version_entry("1.2.0");
        let mut oldest = version_entry("1.1.2");
        oldest.changed = None;
        assert_eq!(
            app(vec![newest, middle, oldest]).describe_history(),
            "code unchanged since 1.2.0"
        );
    }

    #[test]
    fn history_handles_an_app_that_never_changed() {
        let versions = ["1.3.0", "1.2.0", "1.1.2"]
            .iter()
            .map(|v| {
                let mut entry = version_entry(v);
                entry.changed = Some(false);
                entry
            })
            .collect();
        assert_eq!(
            app(versions).describe_history(),
            "code unchanged across 3 releases"
        );
    }

    #[test]
    fn history_admits_when_versions_are_not_comparable() {
        // Different builders produced these, so the byte difference is not
        // evidence about the code and must not be reported as a change.
        let mut newest = version_entry("1.3.0");
        newest.changed = None;
        newest.delta_bytes = None;
        newest.origin = Origin::Kira;
        let mut older = version_entry("1.2.0");
        older.origin = Origin::Upstream;

        let described = app(vec![newest, older]).describe_history();
        assert_eq!(described, "not comparable with 1.2.0");
        assert!(!described.contains("changed"), "{described}");
    }

    #[test]
    fn history_handles_a_single_published_version() {
        let mut only = version_entry("1.3.0");
        only.changed = None;
        assert_eq!(app(vec![only]).describe_history(), "only 1.3.0 published");
    }

    #[test]
    fn releases_sort_newest_first_by_tag_version() {
        let mut releases = ["apps-v1.1.2", "apps-v1.3.0", "apps-v1.2.0"]
            .iter()
            .map(|tag| Release {
                tag: (*tag).to_owned(),
                published_at: Some("2026-06-09T00:00:00Z".into()),
                url: None,
                is_prerelease: false,
                notes: None,
                app_count: 13,
            })
            .collect::<Vec<_>>();
        sort_newest_first(&mut releases);
        let tags: Vec<_> = releases.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(tags, ["apps-v1.3.0", "apps-v1.2.0", "apps-v1.1.2"]);
    }

    #[test]
    fn tags_parsing_to_the_same_version_fall_back_to_date() {
        // Upstream publishes all three of these as full releases.
        let mut releases = [
            ("apps-v0.1.9-rc1", "2026-05-19T00:00:00Z"),
            ("apps-v0.1.9-rc3", "2026-06-02T12:00:00Z"),
            ("apps-v0.1.9-rc2", "2026-06-02T09:00:00Z"),
        ]
        .iter()
        .map(|(tag, when)| Release {
            tag: (*tag).to_owned(),
            published_at: Some((*when).to_owned()),
            url: None,
            is_prerelease: false,
            notes: None,
            app_count: 13,
        })
        .collect::<Vec<_>>();
        sort_newest_first(&mut releases);
        let tags: Vec<_> = releases.iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(
            tags,
            ["apps-v0.1.9-rc3", "apps-v0.1.9-rc2", "apps-v0.1.9-rc1"]
        );
    }

    #[test]
    fn version_is_read_from_any_tag_prefix() {
        assert_eq!(version_from_tag("apps-v1.3.0"), Some(Version::new(1, 3, 0)));
        assert_eq!(
            version_from_tag("apps-v0.1.9-rc3"),
            Some(Version::new(0, 1, 9))
        );
        assert_eq!(version_from_tag("nightly"), None);
    }

    #[test]
    fn a_duplicated_id_drops_every_side_of_the_collision() {
        let entries = vec![
            (AppId::new(0xAAAA), "Alarm"),
            (AppId::new(0xBBBB), "GlanceActivity"),
            (AppId::new(0xBBBB), "GlanceStrain"),
            (AppId::new(0xCCCC), "Running"),
        ];
        let result = partition_unique(entries, |e| e.0, |e| e.1.to_owned());
        let kept: Vec<_> = result.unique.iter().map(|e| e.1).collect();
        assert_eq!(kept, ["Alarm", "Running"]);
        assert_eq!(result.collisions.len(), 1);
        assert_eq!(
            result.collisions[0].labels,
            ["GlanceActivity", "GlanceStrain"]
        );
    }

    #[test]
    fn the_app_with_the_newest_version_keeps_the_folder() {
        let mut current = app(vec![version_entry("1.3.0"), version_entry("1.2.0")]);
        current.app_id = AppId::new(0x8899_AABB_CCDD_EEFF);
        let mut old = app(vec![version_entry("0.1.4")]);
        old.app_id = AppId::new(0xA1F2_9B8D_4E7C_3A65);
        // Same on-device folder, which is the whole problem.
        assert_eq!(current.folder, old.folder);

        let mut apps = vec![old, current];
        mark_superseded(&mut apps);

        let by_id = |id: u64| apps.iter().find(|a| a.app_id == AppId::new(id)).unwrap();
        assert_eq!(by_id(0x8899_AABB_CCDD_EEFF).superseded_by, None);
        assert_eq!(
            by_id(0xA1F2_9B8D_4E7C_3A65).superseded_by,
            Some(AppId::new(0x8899_AABB_CCDD_EEFF))
        );
    }

    #[test]
    fn an_app_alone_in_its_folder_is_never_superseded() {
        let mut apps = vec![app(vec![version_entry("1.3.0")])];
        mark_superseded(&mut apps);
        assert_eq!(apps[0].superseded_by, None);
    }

    #[test]
    fn duplicate_display_names_are_reported() {
        let mut second = app(vec![version_entry("0.1.9")]);
        second.app_id = AppId::new(0xDEAD_BEEF);
        let c = catalog(vec![app(vec![version_entry("1.3.0")]), second]);
        assert_eq!(c.ambiguous_names(), ["Live HR"]);
    }

    #[test]
    fn a_withdrawn_app_withdraws_every_version_of_itself() {
        let mut retired = app(vec![version_entry("1.1.0"), version_entry("1.0.0")]);
        retired.retired = Some("the sensor it reads was removed in firmware 2.0".into());
        let c = catalog(vec![retired]);

        for pin in [None, Some(Version::new(1, 0, 0))] {
            let pinned = pin.map_or_else(BTreeMap::new, |v: Version| {
                BTreeMap::from([(c.apps[0].app_id, v.to_string())])
            });
            let targets = resolve_targets(&c, &pinned);
            assert_eq!(
                targets[0].retired.as_deref(),
                Some("the sensor it reads was removed in firmware 2.0"),
                "pinned to {pin:?}"
            );
        }
    }

    #[test]
    fn one_withdrawn_version_leaves_the_others_on_offer() {
        let mut bad = version_entry("1.1.0");
        bad.retired = Some("writes a corrupt .fit on runs over an hour".into());
        let c = catalog(vec![app(vec![bad, version_entry("1.0.0")])]);

        // The newest is the withdrawn one, so that is what an unpinned selection
        // lands on -- and it must still say so rather than being offered.
        assert!(resolve_targets(&c, &BTreeMap::new())[0].retired.is_some());

        let pinned = BTreeMap::from([(c.apps[0].app_id, "1.0.0".to_owned())]);
        assert_eq!(resolve_targets(&c, &pinned)[0].retired, None);
    }

    #[test]
    fn a_retired_app_does_not_make_a_name_ambiguous() {
        // It is archived, so nothing in the main listing shares the name.
        let mut retired = app(vec![version_entry("1.0.0")]);
        retired.app_id = AppId::new(0xDEAD_BEEF);
        retired.retired = Some("superseded by the Tide Clock 2 app".into());
        let c = catalog(vec![app(vec![version_entry("1.3.0")]), retired]);
        assert!(c.ambiguous_names().is_empty());
    }

    #[test]
    fn a_recipe_names_the_commit_it_was_built_from() {
        let parsed = parse_source(
            "git:https://github.com/someone/una-apps\
             @3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d:tide-clock",
        )
        .expect("a submission's source is parseable");
        assert_eq!(parsed.repo, "https://github.com/someone/una-apps");
        assert_eq!(parsed.rev, "3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d");
        assert_eq!(parsed.subdir, "tide-clock");

        // An SDK app has no repository of its own, and neither does a build with
        // nothing pinned. Say nothing rather than inventing a source.
        assert_eq!(parse_source("sdk:apps-v1.3.0:Examples/Apps/Alarm"), None);
        assert_eq!(parse_source("unpinned"), None);
        assert_eq!(parse_source("git:https://example.test/x"), None);
        assert_eq!(parse_source("git:@abc:."), None);
    }

    #[test]
    fn a_superseded_twin_does_not_make_a_name_ambiguous() {
        // Once the old identity is archived, nothing in the main listing shares
        // the name, so cards there need no disambiguation.
        let mut archived = app(vec![version_entry("0.1.4")]);
        archived.app_id = AppId::new(0xDEAD_BEEF);
        archived.superseded_by = Some(AppId::new(0xA135_8F7C_2E9D_4BA6));
        let c = catalog(vec![app(vec![version_entry("1.3.0")]), archived]);
        assert!(c.ambiguous_names().is_empty());
    }
}
