//! Third-party app submissions: one manifest per app, checked before it is built.
//!
//! Kira builds every binary it ships, so a submission is a *pointer to source*
//! rather than an upload: a repository, a commit, and the SDK revision to compile
//! against. That is the whole trust model — there is nothing to review in a
//! binary nobody can reproduce, and there is no code signing on the watch.
//!
//! Everything here is decided before a build runs, because a build is expensive
//! and because the interesting failures are not build failures. Two apps sharing
//! an `AppID` or an on-device folder is the dangerous one: the watch resolves a
//! folder by loading the first `.uapp` it finds, so a collision can silently boot
//! the wrong app. See [`crate::build_app`] for the checks that happen after.
//!
//! Lives in the CLI, not in `kira-core`: validating submissions is a build-side
//! concern and the browser has no use for it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kira_core::uapp::{AppId, Version};
use serde::Deserialize;

use crate::build_app::flags_id;
use crate::recipe::{Recipe, Wanted};

/// Licences a submission may declare.
///
/// An allowlist rather than a parser: the point is that the source is genuinely
/// available to anyone who wants to check the build, and an unrecognised string
/// cannot be taken as evidence of that. Missing one is a pull request away.
const LICENCES: &[&str] = &[
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "CC0-1.0",
    "GPL-2.0-only",
    "GPL-2.0-or-later",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "ISC",
    "LGPL-2.1-or-later",
    "LGPL-3.0-or-later",
    "MIT",
    "MPL-2.0",
    "Unlicense",
    "Zlib",
];

/// Folder names the watch already uses for something other than an app.
///
/// From the SDK's own layout notes; writing an app into one of these would put it
/// somewhere the launcher does not look, or overwrite shared state.
const RESERVED_FOLDERS: &[&str] = &["SharedData", "ShareData", "System", "Activity"];

/// One published version of a submitted app.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    /// Version to stamp into the binary.
    pub version: Version,
    /// Full commit sha of the source for this version.
    ///
    /// A commit, never a tag or branch: those move, and a moving "pinned" source
    /// would make the recipe a lie.
    pub rev: String,
    /// SDK revision to compile against, e.g. `apps-v1.3.0`.
    pub sdk_rev: String,
    /// Where the app sits in the repository *for this version*, when it has
    /// moved.
    ///
    /// Exists because the path is part of the recipe: with one path for the whole
    /// manifest, rearranging a repository would silently change the recipe of
    /// versions already published, and their artifacts would no longer be the ones
    /// the catalogue describes. Overriding per version lets a monorepo be
    /// reorganised without rewriting history.
    pub subdir: Option<String>,
    /// Why this version was withdrawn, if it was.
    ///
    /// Withdrawn versions stay in the catalogue and keep their binaries: a watch
    /// already carrying one should be recognised and told why, which is more use
    /// than reporting it as something unknown. They are never offered for
    /// installation.
    pub retired: Option<String>,
    /// What changed in this version, in the publisher's own words.
    ///
    /// A submission ships on its own schedule, so no upstream release body ever
    /// mentions it and there is nowhere else for this to come from. Unlike
    /// everything else about a published version this stays editable: it is not
    /// part of the recipe, so correcting a typo changes no artifact and
    /// invalidates no hash. The commit is pinned either way, so the diff remains
    /// the account of record.
    pub notes: Option<String>,
}

/// A submitted app, as declared in `registry/<slug>.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    /// File stem, filled in from the path rather than the file's contents.
    #[serde(skip)]
    pub slug: String,
    /// The app's identity, which must match what its source declares.
    pub app_id: AppId,
    /// Repository holding the source.
    pub source: String,
    /// Path within the repository to the app root, the directory holding
    /// `Software/`. Defaults to the repository root.
    #[serde(default = "dot")]
    pub subdir: String,
    /// Folder to install into under `Apps\` on the watch.
    pub folder: String,
    /// SPDX identifier from [`LICENCES`].
    pub licence: String,
    /// GitHub handle to reach about the submission.
    pub maintainer: String,
    /// Every version to publish, in any order.
    pub versions: Vec<Entry>,
    /// A settings file the app reads from its own folder on the watch.
    ///
    /// Everything else in the catalogue is derived from a binary Kira built
    /// itself. This cannot be: nothing in a `.uapp` says what it reads. So it is
    /// the submitter's word, and it is the one claim the page *acts* on rather
    /// than merely displays — it names a file written to somebody's watch.
    /// Checked by [`kira_core::config::check_spec`] on every catalogue build,
    /// not only when the pull request was reviewed.
    #[serde(default)]
    pub config: Option<kira_core::config::Spec>,
    /// Why the whole app was withdrawn, if it was.
    ///
    /// This is how a listing comes down. Deleting the manifest is not: an app
    /// that vanishes leaves every watch carrying it holding something the
    /// catalogue can no longer name. Deletion is reserved for the cases where the
    /// binaries must genuinely stop being served, which is a maintainer's
    /// decision rather than a submitter's.
    pub retired: Option<String>,
}

impl Manifest {
    /// Why a given version is not on offer, if it is not.
    ///
    /// A withdrawn app withdraws all of its versions; a single version can also
    /// be withdrawn on its own.
    pub(crate) fn retired_for<'a>(&'a self, entry: &'a Entry) -> Option<&'a str> {
        self.retired.as_deref().or(entry.retired.as_deref())
    }

    /// Where a given version's source sits in the repository.
    pub(crate) fn subdir_for<'a>(&'a self, entry: &'a Entry) -> &'a str {
        entry.subdir.as_deref().unwrap_or(&self.subdir)
    }
}

fn dot() -> String {
    ".".to_owned()
}

/// One reason a submission cannot be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Problem {
    /// Which manifest it is about.
    pub slug: String,
    /// What is wrong, phrased for whoever has to fix it.
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.slug, self.message)
    }
}

/// Read one manifest.
///
/// # Errors
/// If the file cannot be read, is not TOML, or is missing a required field.
pub(crate) fn parse(slug: &str, text: &str) -> Result<Manifest> {
    let mut manifest: Manifest =
        toml::from_str(text).with_context(|| format!("{slug}.toml is not a valid manifest"))?;
    slug.clone_into(&mut manifest.slug);
    Ok(manifest)
}

/// Read every `registry/*.toml`, in name order.
///
/// # Errors
/// If the directory cannot be listed or any manifest fails to parse.
pub(crate) fn load_dir(dir: &Path) -> Result<Vec<Manifest>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();

    let mut manifests = Vec::new();
    for path in paths {
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .with_context(|| format!("{} has no usable name", path.display()))?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        manifests.push(parse(slug, &text)?);
    }
    Ok(manifests)
}

/// Whether a string is exactly `len` lowercase hex characters.
fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

/// Check one manifest on its own terms.
fn check_one(manifest: &Manifest, problems: &mut Vec<Problem>) {
    let mut say = |message: String| {
        problems.push(Problem {
            slug: manifest.slug.clone(),
            message,
        });
    };

    if manifest.slug.len() < 2
        || manifest.slug.len() > 40
        || !manifest
            .slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        say(format!(
            "file name {:?} should be 2-40 characters of a-z, 0-9 and -",
            manifest.slug
        ));
    }

    // Only https, and no credentials or query string: the URL is published in the
    // catalogue and fetched by CI, so it has to be plain and inspectable.
    let source = &manifest.source;
    if !source.starts_with("https://") {
        say(format!("source {source:?} must be an https URL"));
    }
    if source.contains('@') || source.contains('?') || source.contains('#') {
        say(format!(
            "source {source:?} must not carry credentials, a query or a fragment"
        ));
    }
    if source.chars().any(char::is_whitespace) {
        say(format!("source {source:?} contains whitespace"));
    }

    for subdir in std::iter::once(&manifest.subdir)
        .chain(manifest.versions.iter().filter_map(|e| e.subdir.as_ref()))
    {
        if subdir.starts_with('/')
            || subdir.contains('\\')
            || subdir.split('/').any(|part| part == "..")
            || subdir.is_empty()
        {
            say(format!(
                "subdir {subdir:?} must be a relative path inside the repository"
            ));
        }
    }

    // The one declaration that is acted on rather than displayed: it names a
    // file the page writes into somebody's watch. Re-checked on every catalogue
    // build, so tightening the rules later catches manifests already merged.
    if let Some(config) = &manifest.config
        && let Err(problem) = kira_core::config::check_spec(config)
    {
        say(format!("config: {problem}"));
    }

    check_folder(manifest, &mut say);
    check_versions(manifest, &mut say);
}

/// Where on the watch a submission wants to live.
fn check_folder(manifest: &Manifest, say: &mut impl FnMut(String)) {
    let folder = &manifest.folder;
    if folder.is_empty()
        || folder.len() > 32
        || !folder
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        say(format!(
            "folder {folder:?} should be 1-32 characters of A-Z, a-z, 0-9, _ and -"
        ));
    }
    if RESERVED_FOLDERS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(folder))
    {
        say(format!("folder {folder:?} is reserved by the watch"));
    }
    if kira_core::fat::is_reserved_device(folder) {
        say(format!("folder {folder:?} is not a usable name on FAT"));
    }
}

/// Licence, maintainer and the versions themselves.
fn check_versions(manifest: &Manifest, say: &mut impl FnMut(String)) {
    if !LICENCES.contains(&manifest.licence.as_str()) {
        say(format!(
            "licence {:?} is not one Kira recognises; open a pull request to add it if it is a real open licence",
            manifest.licence
        ));
    }

    if manifest.maintainer.is_empty()
        || !manifest
            .maintainer
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        say(format!(
            "maintainer {:?} should be a GitHub handle",
            manifest.maintainer
        ));
    }

    if manifest.versions.is_empty() {
        say("no versions listed, so there is nothing to build".to_owned());
    }

    // A bare flag would tell whoever finds the app nothing about why.
    for (what, reason) in std::iter::once(("this app", manifest.retired.as_ref())).chain(
        manifest
            .versions
            .iter()
            .map(|e| ("this version", e.retired.as_ref())),
    ) {
        if let Some(reason) = reason
            && (reason.trim().len() < 8 || reason.len() > 200)
        {
            say(format!(
                "the reason {what} was retired should say why in 8 to 200 characters, not {reason:?}"
            ));
        }
    }

    // Long enough to say something, short enough to stay a note rather than
    // becoming a page. Whoever reads it is deciding whether to install.
    for entry in &manifest.versions {
        if let Some(notes) = &entry.notes
            && (notes.trim().len() < 4 || notes.len() > 500)
        {
            say(format!(
                "version {}: notes should say what changed in 4 to 500 characters, not {notes:?}",
                entry.version
            ));
        }
    }

    let mut seen: BTreeSet<Version> = BTreeSet::new();
    for entry in &manifest.versions {
        if !is_hex(&entry.rev, 40) {
            say(format!(
                "version {}: rev {:?} must be a full 40-character commit sha, not a tag or branch",
                entry.version, entry.rev
            ));
        }
        if !entry.sdk_rev.starts_with("apps-v") {
            say(format!(
                "version {}: sdk_rev {:?} should name an SDK release tag, e.g. apps-v1.3.0",
                entry.version, entry.sdk_rev
            ));
        }
        if !seen.insert(entry.version) {
            say(format!(
                "version {} is listed more than once",
                entry.version
            ));
        }
    }
}

/// Who already holds an identity the catalogue publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Owner {
    /// Display name, for the message.
    pub name: String,
    /// Source repository, when the holder is itself a submission.
    ///
    /// This is what tells a manifest's own published listing apart from someone
    /// else's: an SDK app has no repository of its own, so it is never a match.
    pub repo: Option<String>,
}

impl Owner {
    /// Whether this is the listing `manifest` itself produced.
    ///
    /// A submission keeps the identity it already published under — otherwise
    /// adding a second version would be refused for colliding with its own
    /// first, which is exactly what happened the moment one was published.
    fn is_own_listing(&self, manifest: &Manifest) -> bool {
        self.repo.as_deref() == Some(manifest.source.as_str())
    }
}

/// Check a whole registry, plus what is already published.
///
/// `taken_ids` and `taken_folders` are the identities the catalogue already uses,
/// so a submission cannot claim one — except the one it published itself.
/// Returns every problem rather than the first: a contributor should get the
/// whole list in one round trip.
pub(crate) fn validate(
    manifests: &[Manifest],
    taken_ids: &BTreeMap<AppId, Owner>,
    taken_folders: &BTreeMap<String, Owner>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    for manifest in manifests {
        check_one(manifest, &mut problems);
    }

    // Two apps in one folder is the failure that reaches hardware: the watch
    // loads whichever `.uapp` it finds first, so it could boot either.
    let mut ids: BTreeMap<AppId, &str> = BTreeMap::new();
    let mut folders: BTreeMap<String, &str> = BTreeMap::new();
    for manifest in manifests {
        if let Some(owner) = taken_ids
            .get(&manifest.app_id)
            .filter(|owner| !owner.is_own_listing(manifest))
        {
            problems.push(Problem {
                slug: manifest.slug.clone(),
                message: format!(
                    "AppID {} already belongs to {}; generate a different one",
                    manifest.app_id, owner.name
                ),
            });
        }
        if let Some(first) = ids.insert(manifest.app_id, &manifest.slug) {
            problems.push(Problem {
                slug: manifest.slug.clone(),
                message: format!("AppID {} is also claimed by {first}", manifest.app_id),
            });
        }

        let folder_key = manifest.folder.to_ascii_lowercase();
        if let Some(owner) = taken_folders
            .get(&folder_key)
            .filter(|owner| !owner.is_own_listing(manifest))
        {
            problems.push(Problem {
                slug: manifest.slug.clone(),
                message: format!(
                    "folder {} is already used by {}; the watch cannot hold two apps in one folder",
                    manifest.folder, owner.name
                ),
            });
        }
        if let Some(first) = folders.insert(folder_key, &manifest.slug) {
            problems.push(Problem {
                slug: manifest.slug.clone(),
                message: format!("folder {} is also claimed by {first}", manifest.folder),
            });
        }
    }
    problems
}

/// Check that nothing already published has been rewritten.
///
/// A version's source is fixed once it ships: changing the commit under a version
/// already on someone's watch would make the published hash describe bytes nobody
/// can rebuild. New versions are the way to change anything.
pub(crate) fn check_unchanged(
    before: &[Manifest],
    after: &[Manifest],
    published: &BTreeMap<AppId, Owner>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    let index: BTreeMap<&str, &Manifest> = after.iter().map(|m| (m.slug.as_str(), m)).collect();

    for old in before {
        // Nothing to protect: this app has never reached the catalogue, so no
        // watch can be carrying it and no published hash describes it. Accepting
        // a manifest is not the same as having published it.
        if !published.contains_key(&old.app_id) {
            continue;
        }
        let Some(new) = index.get(old.slug.as_str()) else {
            problems.push(Problem {
                slug: old.slug.clone(),
                message: "manifest was removed; retire an app by adding versions, not by \
                          deleting it, so a watch carrying it is still recognised"
                    .to_owned(),
            });
            continue;
        };

        if new.app_id != old.app_id {
            problems.push(Problem {
                slug: old.slug.clone(),
                message: format!(
                    "AppID changed from {} to {}, which makes this a different app",
                    old.app_id, new.app_id
                ),
            });
        }
        if new.source != old.source {
            problems.push(Problem {
                slug: old.slug.clone(),
                message: format!("source changed from {} to {}", old.source, new.source),
            });
        }

        for entry in &old.versions {
            match new.versions.iter().find(|e| e.version == entry.version) {
                None => problems.push(Problem {
                    slug: old.slug.clone(),
                    message: format!("published version {} was removed", entry.version),
                }),
                Some(now)
                    if now.rev != entry.rev
                        || now.sdk_rev != entry.sdk_rev
                        || new.subdir_for(now) != old.subdir_for(entry) =>
                {
                    problems.push(Problem {
                        slug: old.slug.clone(),
                        message: format!(
                            "version {} was already published from {} at {}; publish a new \
                             version instead of repointing this one — if the app moved, set \
                             subdir on the new version and leave this one alone",
                            entry.version,
                            entry.rev,
                            old.subdir_for(entry)
                        ),
                    });
                }
                Some(_) => {}
            }
        }
    }
    problems
}

impl Manifest {
    /// This manifest's versions, newest first.
    ///
    /// The order the catalogue wants, and the order worth building in.
    pub(crate) fn newest_first(&self) -> Vec<Entry> {
        let mut versions = self.versions.clone();
        versions.sort_by_key(|entry| std::cmp::Reverse(entry.version));
        versions
    }

    /// How one of this manifest's versions is built.
    ///
    /// Shared with the catalogue build, so the artifact it looks for in the store
    /// is by construction the one the submission workflow put there.
    pub(crate) fn recipe_for(&self, entry: &Entry, toolchain: &str) -> Recipe {
        Recipe {
            app_source: app_source(&self.source, &entry.rev, self.subdir_for(entry)),
            sdk_rev: entry.sdk_rev.clone(),
            toolchain: toolchain.to_owned(),
            build_version: entry.version,
            flags: flags_id(),
        }
    }
}

/// Everything a submission needs built, newest version first.
pub(crate) fn wanted(manifests: &[Manifest], toolchain: &str) -> Vec<Wanted> {
    let mut wanted = Vec::new();
    for manifest in manifests {
        for entry in manifest.newest_first() {
            wanted.push(Wanted {
                app_id: manifest.app_id,
                folder: manifest.folder.clone(),
                retired: manifest.retired_for(&entry).map(ToOwned::to_owned),
                recipe: manifest.recipe_for(&entry, toolchain),
            });
        }
    }
    wanted
}

/// Canonical identity of a submitted app's source, for the recipe.
pub(crate) fn app_source(source: &str, rev: &str, subdir: &str) -> String {
    format!("git:{source}@{rev}:{subdir}")
}

/// Render problems as a report, or say there were none.
pub(crate) fn report(problems: &[Problem]) -> String {
    if problems.is_empty() {
        return "no problems found\n".to_owned();
    }
    let mut out = format!(
        "{} problem{}:\n",
        problems.len(),
        if problems.len() == 1 { "" } else { "s" }
    );
    for problem in problems {
        let _ = writeln!(out, "  {problem}");
    }
    out
}

/// Identities a published catalogue already occupies.
///
/// # Errors
/// If the catalogue cannot be read or parsed.
pub(crate) fn taken_from_catalog(
    path: &Path,
) -> Result<(BTreeMap<AppId, Owner>, BTreeMap<String, Owner>)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let catalog: kira_core::catalog::Catalog =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;

    // A schema behind is normal rather than an error: this tool is always
    // deployed before the catalogue it goes on to build, so between a schema
    // bump landing and the next publish the live catalogue is the older one. The
    // three fields read here have been present throughout, and everything added
    // since is optional. Newer is refused, since it may mean something this build
    // does not know about.
    let expected = kira_core::catalog::SCHEMA;
    if catalog.schema > expected {
        bail!(
            "{} is schema {}, newer than the {expected} this build understands",
            path.display(),
            catalog.schema,
        );
    }
    if catalog.schema < expected {
        eprintln!(
            "note: {} is schema {}, one the published site has not caught up from; \
             reading the identities it does carry",
            path.display(),
            catalog.schema,
        );
    }

    let mut ids = BTreeMap::new();
    let mut folders = BTreeMap::new();
    for app in &catalog.apps {
        let owner = Owner {
            name: app.name.clone(),
            repo: app.publisher.as_ref().map(|p| p.repo.clone()),
        };
        ids.insert(app.app_id, owner.clone());
        folders.insert(app.folder.to_ascii_lowercase(), owner);
    }
    Ok((ids, folders))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
app_id = "A7C31F0E9B482D65"
source = "https://github.com/someone/una-tide-clock"
subdir = "."
folder = "TideClock"
licence = "MIT"
maintainer = "someone"

[[versions]]
version = "1.0.0"
rev = "3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d"
sdk_rev = "apps-v1.3.0"
"#;

    fn good() -> Manifest {
        parse("tide-clock", GOOD).unwrap()
    }

    /// The catalogue already carrying the example app.
    fn live() -> BTreeMap<AppId, Owner> {
        BTreeMap::from([(good().app_id, owner("Tide Clock", None))])
    }

    /// An identity holder in the published catalogue.
    fn owner(name: &str, repo: Option<&str>) -> Owner {
        Owner {
            name: name.to_owned(),
            repo: repo.map(ToOwned::to_owned),
        }
    }

    fn checked(manifest: &Manifest) -> Vec<String> {
        let mut problems = Vec::new();
        check_one(manifest, &mut problems);
        problems.into_iter().map(|p| p.message).collect()
    }

    #[test]
    fn a_well_formed_manifest_is_accepted() {
        let manifest = good();
        assert_eq!(manifest.slug, "tide-clock");
        assert_eq!(manifest.app_id.to_string(), "A7C31F0E9B482D65");
        assert_eq!(manifest.versions[0].version, Version::new(1, 0, 0));
        assert!(checked(&manifest).is_empty(), "{:?}", checked(&manifest));
        assert!(validate(&[manifest], &BTreeMap::new(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn subdir_defaults_to_the_repository_root() {
        let text = GOOD.replace("subdir = \".\"\n", "");
        assert_eq!(parse("tide-clock", &text).unwrap().subdir, ".");
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        // A typo in a field name must not silently mean "default".
        let text = format!("{GOOD}\nsandbox = false\n");
        assert!(parse("tide-clock", &text).is_err());
    }

    #[test]
    fn a_moving_reference_is_not_a_pinned_source() {
        for rev in [
            "v1.0.0",
            "main",
            "3f9a1c8",
            "3F9A1C8E5D2B7046AF13C9E8B25D704A6F1C8E3D",
        ] {
            let text = GOOD.replace("3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d", rev);
            let manifest = parse("tide-clock", &text).unwrap();
            let problems = checked(&manifest);
            assert!(
                problems.iter().any(|p| p.contains("commit sha")),
                "{rev} should be refused, got {problems:?}"
            );
        }
    }

    #[test]
    fn a_source_url_must_be_plain_https() {
        for source in [
            "http://github.com/someone/app",
            "git@github.com:someone/app.git",
            "https://user:pw@github.com/someone/app",
            "https://github.com/someone/app?ref=main",
        ] {
            let text = GOOD.replace("https://github.com/someone/una-tide-clock", source);
            let manifest = parse("tide-clock", &text).unwrap();
            assert!(!checked(&manifest).is_empty(), "{source} should be refused");
        }
    }

    #[test]
    fn a_subdir_cannot_escape_the_repository() {
        for subdir in ["../elsewhere", "/etc", "apps\\one", "a/../../b"] {
            let text = GOOD.replace("subdir = \".\"", &format!("subdir = {subdir:?}"));
            let manifest = parse("tide-clock", &text).unwrap();
            assert!(
                checked(&manifest).iter().any(|p| p.contains("subdir")),
                "{subdir} should be refused"
            );
        }
    }

    #[test]
    fn a_folder_must_be_usable_on_the_watch() {
        for folder in ["", "SharedData", "System", "NUL", "has space", "with/slash"] {
            let text = GOOD.replace("folder = \"TideClock\"", &format!("folder = {folder:?}"));
            let manifest = parse("tide-clock", &text).unwrap();
            assert!(
                checked(&manifest).iter().any(|p| p.contains("folder")),
                "{folder:?} should be refused"
            );
        }
    }

    #[test]
    fn a_licence_must_be_one_kira_recognises() {
        let text = GOOD.replace("licence = \"MIT\"", "licence = \"All rights reserved\"");
        let manifest = parse("tide-clock", &text).unwrap();
        assert!(checked(&manifest).iter().any(|p| p.contains("licence")));
    }

    #[test]
    fn an_identity_the_catalogue_already_uses_is_refused() {
        let manifest = good();
        let taken_id = BTreeMap::from([(manifest.app_id, owner("Live HR", None))]);
        let problems = validate(std::slice::from_ref(&manifest), &taken_id, &BTreeMap::new());
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("already belongs"))
        );

        let taken_folder = BTreeMap::from([("tideclock".to_owned(), owner("Alarm", None))]);
        let problems = validate(&[manifest], &BTreeMap::new(), &taken_folder);
        assert!(problems.iter().any(|p| p.message.contains("already used")));
    }

    #[test]
    fn two_submissions_cannot_share_an_identity() {
        let first = good();
        let mut second = good();
        second.slug = "other".into();
        // Same AppID and same folder as the first.
        let problems = validate(
            &[first.clone(), second.clone()],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        assert!(problems.iter().any(|p| p.message.contains("also claimed")));
        assert_eq!(
            problems.len(),
            2,
            "both the id and the folder collide: {problems:?}"
        );

        // Distinct identities are fine.
        second.app_id = AppId::new(0x1234_5678_9ABC_DEF0);
        second.folder = "Other".into();
        assert!(validate(&[first, second], &BTreeMap::new(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_published_version_cannot_be_repointed() {
        let before = vec![good()];
        let mut after = good();
        after.versions[0].rev = "0".repeat(40);
        let problems = check_unchanged(&before, &[after], &live());
        assert_eq!(problems.len(), 1);
        assert!(problems[0].message.contains("publish a new version"));
    }

    #[test]
    fn adding_a_version_is_allowed_and_removing_one_is_not() {
        let before = vec![good()];
        let mut after = good();
        after.versions.push(Entry {
            version: Version::new(1, 1, 0),
            rev: "a".repeat(40),
            sdk_rev: "apps-v1.3.0".into(),
            subdir: None,
            retired: None,
            notes: None,
        });
        assert!(check_unchanged(&before, &[after], &live()).is_empty());

        let mut emptied = good();
        emptied.versions.clear();
        let problems = check_unchanged(&before, &[emptied], &live());
        assert!(problems.iter().any(|p| p.message.contains("was removed")));
    }

    /// A settings declaration is optional, and most manifests have none.
    #[test]
    fn a_manifest_without_settings_is_still_valid() {
        assert!(good().config.is_none());
        assert!(checked(&good()).is_empty());
    }

    /// The declaration names a file the page writes to a device, so a manifest
    /// that would send it outside the app's own folder has to fail the same
    /// check that everything else does — and fail it on every build, not only
    /// on the pull request that introduced it.
    #[test]
    fn a_settings_file_that_escapes_the_app_folder_is_refused() {
        let manifest = parse(
            "tide-clock",
            &GOOD.replace(
                "[[versions]]",
                "[config]\nfile = \"../../../evil.json\"\nschema = 1\n\n\
                 [[config.fields]]\npath = \"values.id\"\ntitle = \"Id\"\nmaxLength = 8\n\n\
                 [[versions]]",
            ),
        )
        .expect("parses");
        let problems = checked(&manifest);
        assert!(
            problems.iter().any(|p| p.contains("config:")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_well_formed_settings_declaration_passes() {
        let manifest = parse(
            "tide-clock",
            &GOOD.replace(
                "[[versions]]",
                "[config]\nfile = \"input.json\"\nschema = 1\n\n\
                 [[config.fields]]\npath = \"values.id\"\ntitle = \"Id\"\nmaxLength = 8\n\n\
                 [[versions]]",
            ),
        )
        .expect("parses");
        assert!(checked(&manifest).is_empty());
        assert_eq!(manifest.config.expect("declared").file, "input.json");
    }

    #[test]
    fn a_manifest_cannot_be_deleted_or_have_its_identity_swapped() {
        let before = vec![good()];
        assert!(
            check_unchanged(&before, &[], &live())[0]
                .message
                .contains("was removed")
        );

        let mut after = good();
        after.app_id = AppId::new(0xDEAD_BEEF_DEAD_BEEF);
        assert!(
            check_unchanged(&before, &[after], &live())
                .iter()
                .any(|p| p.message.contains("different app"))
        );
    }

    #[test]
    fn moving_an_app_within_a_repository_needs_a_new_version() {
        // The path is part of the recipe, so changing it for a published version
        // would leave the catalogue describing an artifact nobody can rebuild.
        let before = vec![good()];
        let mut moved = good();
        moved.subdir = "apps/tide-clock".into();
        let problems = check_unchanged(&before, &[moved], &live());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.contains("if the app moved"));

        // The way through is a per-version override, leaving history alone.
        let mut after = good();
        after.subdir = "apps/tide-clock".into();
        after.versions[0].subdir = Some(".".into());
        after.versions.push(Entry {
            version: Version::new(1, 1, 0),
            rev: "c".repeat(40),
            sdk_rev: "apps-v1.3.0".into(),
            subdir: None,
            retired: None,
            notes: None,
        });
        assert!(check_unchanged(&before, &[after.clone()], &live()).is_empty());

        // And each version builds from where it actually lived.
        let items = wanted(&[after], "sha256:abc");
        assert!(items[0].recipe.app_source.ends_with(":apps/tide-clock"));
        assert!(items[1].recipe.app_source.ends_with(":."));
    }

    #[test]
    fn a_per_version_path_cannot_escape_the_repository_either() {
        let mut manifest = good();
        manifest.versions[0].subdir = Some("../elsewhere".into());
        assert!(checked(&manifest).iter().any(|p| p.contains("subdir")));
    }

    #[test]
    fn a_listing_comes_down_by_being_retired_rather_than_deleted() {
        let before = vec![good()];

        // Withdrawing the app is an accepted change, unlike deleting it.
        let mut retired = good();
        retired.retired = Some("superseded by the Tide Clock 2 app".into());
        assert!(check_unchanged(&before, &[retired.clone()], &live()).is_empty());
        assert_eq!(
            retired.retired_for(&retired.versions[0]),
            Some("superseded by the Tide Clock 2 app")
        );

        // Deleting it is still refused, so a watch carrying it stays nameable.
        assert!(
            check_unchanged(&before, &[], &live())[0]
                .message
                .contains("was removed")
        );

        // And a retired app is still buildable, so its binary can be recognised.
        assert_eq!(wanted(&[retired], "sha256:abc").len(), 1);
    }

    #[test]
    fn one_version_can_be_withdrawn_without_the_others() {
        let mut manifest = good();
        manifest.versions[0].retired = Some("writes a corrupt .fit on long runs".into());
        manifest.versions.push(Entry {
            version: Version::new(1, 1, 0),
            rev: "d".repeat(40),
            sdk_rev: "apps-v1.3.0".into(),
            subdir: None,
            retired: None,
            notes: None,
        });
        assert!(checked(&manifest).is_empty(), "{:?}", checked(&manifest));
        assert!(manifest.retired_for(&manifest.versions[0]).is_some());
        assert!(manifest.retired_for(&manifest.versions[1]).is_none());
    }

    #[test]
    fn withdrawing_something_has_to_say_why() {
        for reason in ["", "  ", "bad", &"x".repeat(201)] {
            let mut manifest = good();
            manifest.retired = Some(reason.to_owned());
            assert!(
                checked(&manifest).iter().any(|p| p.contains("retired")),
                "{reason:?} should be refused"
            );
        }
        let mut manifest = good();
        manifest.retired = Some("the sensor it reads was removed in firmware 2.0".into());
        assert!(checked(&manifest).is_empty());
    }

    #[test]
    fn an_app_that_never_reached_the_catalogue_can_be_withdrawn_outright() {
        // Accepting a submission is not publishing it. Until an app is in the
        // catalogue there is no watch carrying it and no hash describing it, so
        // taking the manifest back out again costs nobody anything.
        let before = vec![good()];
        assert!(check_unchanged(&before, &[], &BTreeMap::new()).is_empty());

        // Once published, it has to be retired instead.
        assert!(
            check_unchanged(&before, &[], &live())[0]
                .message
                .contains("was removed")
        );
    }

    #[test]
    fn a_catalogue_a_schema_behind_still_answers_which_identities_are_taken() {
        // The state on every schema bump: the tool ships first, and the site is
        // still serving what the previous build produced. Refusing to read it
        // would fail the submission checks until the catalogue caught up.
        let dir = std::env::temp_dir().join("kira-registry-schema-test");
        std::fs::create_dir_all(&dir).unwrap();

        let older = kira_core::catalog::SCHEMA - 1;
        let path = dir.join("old.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"schema": {older}, "generated": "x", "source": {{"repo": null}},
                     "releases": [], "apps": [{{"appId": "A19C2A7E4F8B6D31", "name": "Alarm",
                     "type": "Utility", "folder": "Alarm", "versions": []}}]}}"#
            ),
        )
        .unwrap();
        let (ids, folders) = taken_from_catalog(&path).unwrap();
        assert_eq!(ids[&AppId::new(0xA19C_2A7E_4F8B_6D31)].name, "Alarm");
        assert_eq!(folders["alarm"].name, "Alarm");

        // Newer is refused: it may mean something this build cannot see.
        let newer = dir.join("new.json");
        let bumped = kira_core::catalog::SCHEMA + 1;
        std::fs::write(
            &newer,
            format!(
                r#"{{"schema": {bumped}, "generated": "x", "source": {{"repo": null}},
                     "releases": [], "apps": []}}"#
            ),
        )
        .unwrap();
        let err = taken_from_catalog(&newer).unwrap_err().to_string();
        assert!(err.contains("newer than"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_app_keeps_the_identity_it_already_published_under() {
        // Once a submission reaches the catalogue, its own listing holds its
        // AppID and its folder. Reading those back as "taken" refused every
        // later version of it -- and since validate checks the whole registry,
        // every other submission's pull request along with it. The catalogue
        // entry records which repository it came from, which is what tells a
        // manifest's own listing apart from somebody else's.
        let manifest = good();
        let mine = owner("Tide Clock", Some(&manifest.source));
        let ids = BTreeMap::from([(manifest.app_id, mine.clone())]);
        let folders = BTreeMap::from([("tideclock".to_owned(), mine)]);
        assert!(
            validate(std::slice::from_ref(&manifest), &ids, &folders).is_empty(),
            "a submission must not collide with its own listing"
        );

        // Someone else's submission holding it is still a collision.
        let theirs = owner("Impostor", Some("https://github.com/someone-else/app"));
        let ids = BTreeMap::from([(manifest.app_id, theirs)]);
        assert!(!validate(&[manifest], &ids, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_version_can_say_what_it_changed() {
        let mut manifest = good();
        manifest.versions[0].notes = Some("The back button now exits the app.".into());
        assert!(checked(&manifest).is_empty(), "{:?}", checked(&manifest));

        for notes in ["", "  ", "x", &"x".repeat(501)] {
            let mut manifest = good();
            manifest.versions[0].notes = Some(notes.to_owned());
            assert!(
                checked(&manifest).iter().any(|p| p.contains("notes")),
                "{notes:?} should be refused"
            );
        }
    }

    #[test]
    fn notes_stay_editable_after_a_version_is_published() {
        // Unlike rev, sdk_rev and subdir, notes are not part of the recipe, so
        // rewording them changes no artifact and invalidates no published hash.
        // The commit stays pinned, so the diff remains the account of record.
        let before = vec![good()];
        let mut after = good();
        after.versions[0].notes = Some("Reworded after the fact.".into());
        assert!(check_unchanged(&before, &[after], &live()).is_empty());
    }

    #[test]
    fn a_recipe_records_the_exact_commit() {
        let items = wanted(&[good()], "sha256:abc");
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].recipe.app_source,
            "git:https://github.com/someone/una-tide-clock\
             @3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d:."
        );
        assert_eq!(items[0].recipe.sdk_rev, "apps-v1.3.0");
        assert_eq!(items[0].folder, "TideClock");
        // Two versions of one app are two different artifacts.
        let mut two = good();
        two.versions.push(Entry {
            version: Version::new(1, 1, 0),
            rev: "b".repeat(40),
            sdk_rev: "apps-v1.3.0".into(),
            subdir: None,
            retired: None,
            notes: None,
        });
        let items = wanted(&[two], "sha256:abc");
        assert_ne!(items[0].recipe.key(), items[1].recipe.key());
        // Newest first.
        assert_eq!(items[0].recipe.build_version, Version::new(1, 1, 0));
    }

    #[test]
    fn the_report_lists_every_problem_at_once() {
        let mut manifest = good();
        manifest.licence = "nope".into();
        manifest.folder = "System".into();
        let problems = validate(&[manifest], &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(problems.len(), 2);
        let text = report(&problems);
        assert!(text.starts_with("2 problems:"));
        assert!(text.contains("tide-clock: "));
        assert_eq!(report(&[]), "no problems found\n");
    }
}
