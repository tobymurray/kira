//! Building the published catalogue from unzipped releases.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sha256_hex;
use anyhow::{Context, Result, bail, ensure};
use kira_core::catalog::{
    App, BuiltFrom, Catalog, Origin, Publisher, Release, ReleaseOrder, SCHEMA, Source,
    VersionEntry, partition_unique, sort_newest_first,
};
use kira_core::icon;
use kira_core::uapp::{AppId, Header, Uapp, Version};

use crate::build_app::flags_id;
use crate::recipe::Recipe;
use crate::registry::{self, Manifest};
use serde::Deserialize;

/// Command-line inputs for a build.
#[derive(Debug)]
pub(crate) struct Args {
    /// Directory holding the unzipped release(s).
    pub src: PathBuf,
    /// Site directory to write `data/` into.
    pub out: PathBuf,
    /// Optional release metadata.
    pub releases: Option<PathBuf>,
    /// Upstream repository.
    pub repo: Option<String>,
    /// Tag to assume for a single-release source.
    pub tag: Option<String>,
    /// Directory of binaries Kira has already built, named by recipe.
    ///
    /// Any version with a build here is served from it; the rest fall back to
    /// upstream's binary, which `origin` records.
    pub built: Option<PathBuf>,
    /// Toolchain identity the store was built with. Required with `--built`,
    /// since the recipe -- and therefore the artifact name -- depends on it.
    pub toolchain: Option<String>,
    /// Directory of third-party submission manifests, to publish beside the SDK's
    /// apps. Needs `--built`: a submission has no vendor binary to fall back on.
    pub registry: Option<PathBuf>,
}

/// Release metadata as fetched from the GitHub API by the workflow.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseMeta {
    tag: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    is_prerelease: bool,
    #[serde(default)]
    notes: Option<String>,
}

/// A release directory paired with whatever metadata was supplied for it.
#[derive(Debug)]
struct ReleaseDir {
    tag: String,
    dir: PathBuf,
    meta: Option<ReleaseMeta>,
}

impl ReleaseOrder for ReleaseDir {
    fn tag(&self) -> &str {
        &self.tag
    }

    fn published_at(&self) -> Option<&str> {
        self.meta.as_ref()?.published_at.as_deref()
    }
}

/// One app's binary within a release.
struct Binary {
    folder: String,
    file: String,
    bytes: Vec<u8>,
    /// `AppID` as upstream's binary reports it, to cross-check Kira's build.
    app_id_hint: AppId,
}

/// Directory entries that are directories, sorted by name.
fn subdirs(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn uapps_in(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.to_lowercase().ends_with(".uapp") {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Find `<root>/<Folder>/<one>.uapp`, rejecting ambiguous folders.
fn discover_binaries(root: &Path) -> Result<Vec<Binary>> {
    let mut found = Vec::new();
    for folder in subdirs(root)? {
        let dir = root.join(&folder);
        let uapps = uapps_in(&dir)?;
        match uapps.as_slice() {
            [] => {}
            [file] => {
                let path = dir.join(file);
                let bytes =
                    fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
                let app_id_hint = Uapp::parse(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?
                    .header()
                    .app_id;
                found.push(Binary {
                    folder,
                    file: file.clone(),
                    bytes,
                    app_id_hint,
                });
            }
            // The watch loads the FIRST .uapp it finds in a folder, so shipping
            // two is never right — refuse rather than pick arbitrarily.
            many => bail!(
                "{folder}: {} .uapp files, expected 1: {}",
                many.len(),
                many.join(", ")
            ),
        }
    }
    Ok(found)
}

/// A single release directly under `--src`, or one directory per release tag?
fn is_single_release(src: &Path) -> Result<bool> {
    for child in subdirs(src)? {
        if !uapps_in(&src.join(child))?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn release_dirs(args: &Args) -> Result<Vec<ReleaseDir>> {
    let metas: Vec<ReleaseMeta> = match &args.releases {
        Some(path) => {
            let raw =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str(&raw).context("--releases must be a JSON array of releases")?
        }
        None => Vec::new(),
    };
    let by_tag: BTreeMap<&str, &ReleaseMeta> = metas.iter().map(|m| (m.tag.as_str(), m)).collect();

    let mut dirs = if is_single_release(&args.src)? {
        let tag = args.tag.clone().unwrap_or_else(|| "unversioned".to_owned());
        vec![ReleaseDir {
            meta: by_tag.get(tag.as_str()).map(|m| (*m).clone()),
            tag,
            dir: args.src.clone(),
        }]
    } else {
        subdirs(&args.src)?
            .into_iter()
            .map(|tag| ReleaseDir {
                meta: by_tag.get(tag.as_str()).map(|m| (*m).clone()),
                dir: args.src.join(&tag),
                tag,
            })
            .collect()
    };

    // Newest release first, so each app's version list is newest first too.
    sort_newest_first(&mut dirs);
    Ok(dirs)
}

/// How many versions each origin contributed, for the run summary.
#[derive(Debug, Default)]
struct OriginCounts {
    kira: usize,
    upstream: usize,
    diverged: usize,
    rejected: usize,
}

impl OriginCounts {
    fn record_rejection(&mut self) {
        self.rejected += 1;
    }

    fn record(&mut self, origin: Origin, matches_upstream: bool) {
        match origin {
            Origin::Kira => {
                self.kira += 1;
                if !matches_upstream {
                    self.diverged += 1;
                }
            }
            Origin::Upstream => self.upstream += 1,
        }
    }
}

/// The binary chosen for publication, and what is known about it.
struct Chosen {
    bytes: Vec<u8>,
    payload_sha256: String,
    origin: Origin,
    /// A Kira build existed but was refused, rather than none being found.
    rejected: bool,
    /// Hash of what upstream published for the same app and version.
    upstream_sha256: String,
    /// Whether the served bytes are upstream's bytes.
    matches_upstream: bool,
    built_from: Option<BuiltFrom>,
}

/// Publish Kira's build where one exists, otherwise upstream's.
///
/// A Kira build is accepted only if it agrees with upstream's binary about which
/// app and version it is; a mismatch means the recipe or the label produced the
/// wrong file, which is a bug rather than something to publish.
fn choose_binary(
    built: Option<&BuiltStore>,
    release: &ReleaseDir,
    upstream: &Binary,
    version: Version,
) -> Result<Chosen> {
    let upstream_sha256 = sha256_hex(&upstream.bytes);
    let upstream_payload = sha256_hex(
        Uapp::parse(&upstream.bytes)
            .context("re-parsing the upstream binary")?
            .payload(),
    );

    let found = built.and_then(|store| store.look_up(&release.tag, &upstream.folder, version));
    let Some((recipe, path)) = found else {
        return Ok(Chosen {
            payload_sha256: upstream_payload,
            origin: Origin::Upstream,
            matches_upstream: true,
            upstream_sha256,
            bytes: upstream.bytes.clone(),
            built_from: None,
            rejected: false,
        });
    };

    let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed = Uapp::parse(&bytes).with_context(|| format!("parsing {}", path.display()))?;

    // A build that disagrees with upstream's binary is not necessarily our bug.
    // apps-v0.1.9-rc1 publishes a GlanceFloors with AppID A1C6E4F8A7D92B30 while
    // the source at that tag declares 99AABBCCDDEEFF00, so upstream's binaries
    // there were not built from the source the tag points at. Publishing ours
    // under that version would misattribute it, and aborting would let one old
    // inconsistency block the whole catalogue. Fall back and say so.
    let rejection = if !parsed.verify_crc().is_valid() {
        Some("fails its own CRC".to_owned())
    } else if parsed.header().app_id != upstream.app_id_hint {
        Some(format!(
            "AppID {} but upstream published {} for this app",
            parsed.header().app_id,
            upstream.app_id_hint
        ))
    } else if parsed.header().version != version {
        Some(format!(
            "version {} but {version} was requested",
            parsed.header().version
        ))
    } else {
        None
    };

    if let Some(reason) = rejection {
        eprintln!(
            "  ! {}/{}: not using Kira's build -- {reason}",
            release.tag, upstream.folder
        );
        return Ok(Chosen {
            payload_sha256: upstream_payload,
            origin: Origin::Upstream,
            matches_upstream: true,
            upstream_sha256,
            bytes: upstream.bytes.clone(),
            built_from: None,
            rejected: true,
        });
    }

    let matches_upstream = bytes == upstream.bytes;
    Ok(Chosen {
        payload_sha256: sha256_hex(parsed.payload()),
        origin: Origin::Kira,
        matches_upstream,
        upstream_sha256,
        bytes,
        rejected: false,
        built_from: Some(BuiltFrom {
            app_source: recipe.app_source.clone(),
            sdk_rev: recipe.sdk_rev.clone(),
            toolchain: recipe.toolchain.clone(),
            recipe: recipe.key(),
        }),
    })
}

/// The binaries Kira has already built, as a flat directory of artifacts named by
/// recipe.
///
/// Populated by the app-binaries workflow; absent during a transition, in which
/// case the pipeline falls back to republishing upstream's binary and says so.
struct BuiltStore {
    dir: PathBuf,
    names: std::collections::BTreeSet<String>,
    toolchain: String,
}

impl BuiltStore {
    fn load(dir: PathBuf, toolchain: String) -> Result<Self> {
        let mut names = std::collections::BTreeSet::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.to_lowercase().ends_with(".uapp") {
                names.insert(name);
            }
        }
        Ok(Self {
            dir,
            names,
            toolchain,
        })
    }

    /// The recipe by which an app in an SDK release would have been built.
    fn recipe(&self, tag: &str, folder: &str, version: Version) -> Recipe {
        Recipe {
            app_source: format!("sdk:{tag}:Examples/Apps/{folder}"),
            sdk_rev: tag.to_owned(),
            toolchain: self.toolchain.clone(),
            build_version: version,
            flags: flags_id(),
        }
    }

    /// Kira's build of an app, if one has been made under the current recipe.
    fn look_up(&self, tag: &str, folder: &str, version: Version) -> Option<(Recipe, PathBuf)> {
        let recipe = self.recipe(tag, folder, version);
        let name = recipe.artifact_name(folder);
        self.names
            .contains(&name)
            .then(|| (recipe, self.dir.join(name)))
    }
}

/// A binary with its header parsed and its hashes computed.
struct Parsed {
    binary: Binary,
    header: kira_core::uapp::Header,
    normal_icon: Vec<u8>,
    small_icon: Vec<u8>,
}

/// Parse every binary in a release, rejecting any whose CRC does not match.
fn parse_release(tag: &str, binaries: Vec<Binary>) -> Result<Vec<Parsed>> {
    binaries
        .into_iter()
        .map(|binary| {
            let uapp = Uapp::parse(&binary.bytes)
                .with_context(|| format!("{tag}/{}/{}", binary.folder, binary.file))?;
            let crc = uapp.verify_crc();
            // A CRC failure means the kernel would silently drop this file. Never
            // publish one: the user would install it and see nothing appear.
            if !crc.is_valid() {
                bail!(
                    "{tag}/{}/{}: CRC mismatch (stored {:#010x}, computed {:#010x}): refusing to publish",
                    binary.folder,
                    binary.file,
                    crc.stored,
                    crc.computed
                );
            }
            Ok(Parsed {
                header: uapp.header().clone(),
                normal_icon: uapp.normal_icon().to_vec(),
                small_icon: uapp.small_icon().to_vec(),
                binary,
            })
        })
        .collect()
}

/// Outcome of reading one release directory.
enum Processed {
    /// The release contributed at least one app.
    Included(Release),
    /// Nothing usable, e.g. every app collided on its [`AppId`].
    Skipped,
}

/// Read one release, writing its binaries and icons and folding its apps into
/// `apps`.
fn process_release(
    release: &ReleaseDir,
    data: &Path,
    apps: &mut BTreeMap<AppId, App>,
    total_bytes: &mut u64,
    built: Option<&BuiltStore>,
    counts: &mut OriginCounts,
) -> Result<Processed> {
    let parsed = parse_release(&release.tag, discover_binaries(&release.dir)?)?;

    // Within one release an AppId must be unique; across releases it repeats by
    // design, which is how versions are grouped.
    let partitioned = partition_unique(
        parsed,
        |entry| entry.header.app_id,
        |entry| entry.binary.folder.clone(),
    );
    for collision in &partitioned.collisions {
        eprintln!("  ! {}: dropped AppID collision — {collision}", release.tag);
    }

    let kept = partitioned.unique.len();
    println!(
        "{}: {kept} apps{}",
        release.tag,
        if partitioned.collisions.is_empty() {
            String::new()
        } else {
            format!(" ({} dropped)", partitioned.collisions.len())
        }
    );
    if kept == 0 {
        return Ok(Processed::Skipped);
    }

    for Parsed {
        binary,
        header,
        normal_icon,
        small_icon,
    } in partitioned.unique
    {
        // Prefer Kira's own build. Falling back to upstream's binary keeps
        // releases that have not been built yet in the catalogue, and `origin`
        // says which it is rather than papering over the difference.
        let chosen = choose_binary(built, release, &binary, header.version)?;
        counts.record(chosen.origin, chosen.matches_upstream);
        if chosen.rejected {
            counts.record_rejection();
        }

        let download = format!("apps/{}/{}/{}", release.tag, binary.folder, binary.file);
        let target = data.join(&download);
        fs::create_dir_all(target.parent().expect("download path has a parent"))?;
        fs::write(&target, &chosen.bytes)
            .with_context(|| format!("writing {}", target.display()))?;
        *total_bytes += chosen.bytes.len() as u64;

        let entry = apps.entry(header.app_id).or_insert_with(|| App {
            app_id: header.app_id,
            name: header.name.clone(),
            app_type: header.app_type(),
            folder: binary.folder.clone(),
            versions: Vec::new(),
            icon: None,
            icon_small: None,
            // Decided once every app is known.
            superseded_by: None,
            // An SDK app's publisher is the catalogue's own source, and upstream
            // withdraws an app by dropping it from a release rather than saying so.
            publisher: None,
            retired: None,
        });

        // Same version published under two tags: keep the newer release's.
        if entry.versions.iter().any(|v| v.version == header.version) {
            continue;
        }

        entry.versions.push(VersionEntry {
            version: header.version,
            version_packed: header.version.packed(),
            tag: release.tag.clone(),
            folder: binary.folder.clone(),
            file: binary.file.clone(),
            libc_version: header.libc_version,
            autostart: header.autostart(),
            size: chosen.bytes.len(),
            sha256: sha256_hex(&chosen.bytes),
            payload_sha256: chosen.payload_sha256,
            download,
            // Filled once every version is known.
            changed: None,
            delta_bytes: None,
            origin: chosen.origin,
            // Recorded whichever binary is served, so a watch carrying the
            // vendor's build can be recognised rather than nagged.
            upstream_sha256: Some(chosen.upstream_sha256),
            matches_upstream: Some(chosen.matches_upstream),
            built_from: chosen.built_from,
            retired: None,
        });

        record_icons(entry, data, header.app_id, &normal_icon, &small_icon)?;
    }

    Ok(Processed::Included(Release {
        tag: release.tag.clone(),
        published_at: release.meta.as_ref().and_then(|m| m.published_at.clone()),
        url: release.meta.as_ref().and_then(|m| m.url.clone()),
        is_prerelease: release.meta.as_ref().is_some_and(|m| m.is_prerelease),
        // Upstream release bodies, verbatim. Rendered as text by the site,
        // never as HTML — this is third-party Markdown.
        notes: release
            .meta
            .as_ref()
            .and_then(|m| m.notes.as_ref())
            .map(|n| n.trim().to_owned()),
        app_count: kept,
    }))
}

/// Extract an app's icons, taking them from the newest version that has any.
///
/// A declared length does not mean there are pixels: Glance apps built with icons
/// off carry a zero-filled field of the full size.
fn record_icons(
    app: &mut App,
    data: &Path,
    app_id: AppId,
    normal: &[u8],
    small: &[u8],
) -> Result<()> {
    for (slot, field, suffix) in [
        (&mut app.icon, normal, ""),
        (&mut app.icon_small, small, "@30"),
    ] {
        if slot.is_some() || field.is_empty() || icon::is_blank(field) {
            continue;
        }
        let decoded = icon::decode(field)?;
        let rel = format!("icons/{app_id}{suffix}.png");
        write_png(&data.join(&rel), &decoded)?;
        *slot = Some(rel);
    }
    Ok(())
}

/// Re-run the submission checks against the catalogue as it stands now.
///
/// The same rules the pull request ran, deliberately run again here. A manifest
/// is only ever checked against the catalogue it was accepted into, and both
/// sides move afterwards: upstream can ship a colliding `AppID` or on-device
/// folder in any later release, and nothing would notice. Both collisions reach
/// hardware -- the id is the app's whole identity to the watch and the phone, and
/// the watch loads whichever `.uapp` it finds first in a folder, so sharing one
/// can mean silently booting the wrong app.
///
/// It also means nothing malformed is ever published, whatever route a manifest
/// took onto `main`. The page links a submission's `source`, so "https and
/// nothing else" has to hold at the point of publication, not only at review.
fn refuse_bad_submissions(manifests: &[Manifest], upstream: &BTreeMap<AppId, App>) -> Result<()> {
    let ids = upstream
        .iter()
        .map(|(id, app)| (*id, app.name.clone()))
        .collect();
    let folders = upstream
        .values()
        .map(|app| (app.folder.to_ascii_lowercase(), app.name.clone()))
        .collect();

    let problems = registry::validate(manifests, &ids, &folders);
    ensure!(
        problems.is_empty(),
        "the registry cannot be published as it stands.\n{}\
         An identity an SDK app already holds risks the watch running the wrong \
         app, so nothing here is published until it is resolved: retire the \
         submission, or give it an identity of its own in a new version.",
        registry::report(&problems)
    );
    Ok(())
}

/// The name a submitted binary is written under on the watch.
///
/// Nothing in the store carries one: artifacts there are named by recipe, which
/// is a digest. The name is not semantic to the watch, which loads whichever
/// `.uapp` is in the folder, so this follows the convention upstream's own
/// binaries use and keeps it path-safe -- a display name is not, `AVG / R HR`
/// being a real one.
fn device_file_name(name: &str, version: Version) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let stem = cleaned.trim_matches('_');
    let stem = if stem.is_empty() { "app" } else { stem };
    format!("{stem}_{version}.uapp")
}

/// Fold the submitted apps into the catalogue, beside the SDK's own.
///
/// Deliberately not a section of its own: who published an app is provenance,
/// which the card states, and a listing that put one source above the other would
/// be ranking something Kira has no way to judge. See `registry/README.md`.
///
/// A version with no artifact in the store is left out rather than described:
/// there is no vendor binary to fall back on, so publishing an entry for it would
/// name a download that does not exist.
fn fold_registry(
    dir: &Path,
    data: &Path,
    apps: &mut BTreeMap<AppId, App>,
    total_bytes: &mut u64,
    built: &BuiltStore,
) -> Result<RegistryCounts> {
    let manifests = registry::load_dir(dir)?;
    refuse_bad_submissions(&manifests, apps)?;

    let mut counts = RegistryCounts {
        manifests: manifests.len(),
        ..RegistryCounts::default()
    };

    for manifest in &manifests {
        for entry in manifest.newest_first() {
            let recipe = manifest.recipe_for(&entry, &built.toolchain);
            let path = built.dir.join(recipe.artifact_name(&manifest.folder));
            if !path.is_file() {
                // The window between a manifest landing on main and its build
                // reaching the store, or a build that failed. Either way there
                // are no bytes to describe.
                eprintln!(
                    "  ! {} {}: no stored artifact, so this version is not published",
                    manifest.slug, entry.version
                );
                counts.missing += 1;
                continue;
            }

            let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let uapp =
                Uapp::parse(&bytes).with_context(|| format!("parsing {}", path.display()))?;
            let header = uapp.header().clone();

            // Kira built this itself from a pinned recipe and verified it then, so
            // a disagreement now is a broken store rather than history to work
            // around. Refuse, instead of publishing something mislabelled.
            let crc = uapp.verify_crc();
            ensure!(
                crc.is_valid(),
                "{}: CRC mismatch (stored {:#010x}, computed {:#010x})",
                path.display(),
                crc.stored,
                crc.computed
            );
            ensure!(
                header.app_id == manifest.app_id,
                "{}: AppID {} but {} declares {}",
                path.display(),
                header.app_id,
                manifest.slug,
                manifest.app_id
            );
            ensure!(
                header.version == entry.version,
                "{}: version {} but {} was requested",
                path.display(),
                header.version,
                entry.version
            );

            let file = device_file_name(&header.name, header.version);
            let download = format!("apps/registry/{}/{}/{file}", manifest.slug, manifest.folder);
            let target = data.join(&download);
            fs::create_dir_all(target.parent().expect("download path has a parent"))?;
            fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
            *total_bytes += bytes.len() as u64;

            let app = apps.entry(manifest.app_id).or_insert_with(|| App {
                app_id: manifest.app_id,
                name: header.name.clone(),
                app_type: header.app_type(),
                folder: manifest.folder.clone(),
                versions: Vec::new(),
                icon: None,
                icon_small: None,
                superseded_by: None,
                publisher: Some(Publisher {
                    repo: manifest.source.clone(),
                    maintainer: manifest.maintainer.clone(),
                }),
                retired: manifest.retired.clone(),
            });

            app.versions.push(submitted_version(SubmittedBuild {
                header: &header,
                entry: &entry,
                recipe: &recipe,
                manifest,
                size: bytes.len(),
                sha256: sha256_hex(&bytes),
                payload_sha256: sha256_hex(uapp.payload()),
                file,
                download,
            }));
            counts.versions += 1;
            if manifest.retired_for(&entry).is_some() {
                counts.retired += 1;
            }

            record_icons(
                app,
                data,
                manifest.app_id,
                uapp.normal_icon(),
                uapp.small_icon(),
            )?;
        }
    }

    Ok(counts)
}

/// One stored submission artifact, ready to become a catalogue version.
struct SubmittedBuild<'a> {
    header: &'a Header,
    entry: &'a registry::Entry,
    recipe: &'a Recipe,
    manifest: &'a Manifest,
    size: usize,
    sha256: String,
    payload_sha256: String,
    file: String,
    download: String,
}

/// One stored submission artifact, as a catalogue version.
fn submitted_version(built: SubmittedBuild<'_>) -> VersionEntry {
    let SubmittedBuild {
        header,
        entry,
        recipe,
        manifest,
        size,
        sha256,
        payload_sha256,
        file,
        download,
    } = built;
    VersionEntry {
        version: header.version,
        version_packed: header.version.packed(),
        // A submission ships on its own schedule, so there is no upstream release
        // it came from; its own manifest is what published it.
        tag: manifest.slug.clone(),
        folder: manifest.folder.clone(),
        file,
        libc_version: header.libc_version,
        autostart: header.autostart(),
        size,
        sha256,
        payload_sha256,
        download,
        // Filled once every version is known.
        changed: None,
        delta_bytes: None,
        origin: Origin::Kira,
        built_from: Some(BuiltFrom {
            app_source: recipe.app_source.clone(),
            sdk_rev: recipe.sdk_rev.clone(),
            toolchain: recipe.toolchain.clone(),
            recipe: recipe.key(),
        }),
        // There is no vendor binary for a submission, so there is nothing to
        // compare against -- unknown, rather than a claim either way.
        upstream_sha256: None,
        matches_upstream: None,
        // The app's own withdrawal is recorded on the app; this is per-version.
        retired: entry.retired.clone(),
    }
}

/// What the submissions contributed, for the run summary.
#[derive(Debug, Default)]
struct RegistryCounts {
    manifests: usize,
    versions: usize,
    missing: usize,
    retired: usize,
}

/// Build the catalogue and write it, plus the binaries and icons, under `out`.
///
/// # Errors
/// Fails on unreadable input, an ambiguous app folder, a `.uapp` whose CRC does
/// not match, or a source with no usable releases at all.
pub(crate) fn run(args: &Args) -> Result<()> {
    let data = args.out.join("data");
    let releases_dirs = release_dirs(args)?;
    if releases_dirs.is_empty() {
        bail!("no releases found under {}", args.src.display());
    }

    // Rebuilt from scratch each time: a stale binary from a withdrawn release
    // must not linger in the published output.
    for stale in ["apps", "icons"] {
        let path = data.join(stale);
        if path.exists() {
            fs::remove_dir_all(&path).with_context(|| format!("clearing {}", path.display()))?;
        }
    }
    fs::create_dir_all(data.join("icons"))?;

    let built = match (args.built.clone(), args.toolchain.clone()) {
        (Some(dir), Some(toolchain)) => {
            let store = BuiltStore::load(dir, toolchain)?;
            println!("store holds {} built binaries", store.names.len());
            Some(store)
        }
        // Without the toolchain the artifact names cannot be computed, and
        // guessing would silently publish upstream binaries as if Kira built them.
        (Some(_), None) => bail!("--built requires --toolchain, which the recipe depends on"),
        _ => None,
    };
    let mut counts = OriginCounts::default();

    let mut apps: BTreeMap<AppId, App> = BTreeMap::new();
    let mut releases: Vec<Release> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut total_bytes: u64 = 0;

    for release in &releases_dirs {
        match process_release(
            release,
            &data,
            &mut apps,
            &mut total_bytes,
            built.as_ref(),
            &mut counts,
        )? {
            Processed::Included(record) => releases.push(record),
            Processed::Skipped => skipped.push(release.tag.clone()),
        }
    }

    if releases.is_empty() {
        bail!("no usable releases: nothing to publish");
    }

    // After the releases, so the collision check sees every upstream identity.
    let submissions = match &args.registry {
        Some(dir) => Some(fold_registry(
            dir,
            &data,
            &mut apps,
            &mut total_bytes,
            // A submission has no vendor binary to fall back on, so without the
            // store there is nothing to publish and nothing to say about it.
            built
                .as_ref()
                .context("--registry requires --built and --toolchain")?,
        )?),
        None => None,
    };

    let mut apps: Vec<App> = apps.into_values().collect();
    for app in &mut apps {
        annotate_history(app);
    }
    // Two apps cannot share an on-device folder, and upstream's reassigned ids
    // leave three pairs that do. Decided after history, since it compares the
    // newest version of each.
    kira_core::catalog::mark_superseded(&mut apps);

    // Case-insensitive, then exact, so the order is stable across machines.
    // JavaScript's localeCompare depends on the host locale, which made the
    // previous implementation's ordering environment-dependent.
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    let catalog = Catalog {
        schema: SCHEMA,
        generated: now_rfc3339_millis(),
        source: Source {
            repo: args.repo.clone(),
        },
        releases,
        apps,
    };

    let json = serde_json::to_string_pretty(&catalog)?;
    fs::write(data.join("catalog.json"), format!("{json}\n"))?;
    report(
        &catalog,
        &counts,
        submissions.as_ref(),
        &skipped,
        total_bytes,
        &data,
    );
    Ok(())
}

/// Print what the run produced.
fn report(
    catalog: &Catalog,
    counts: &OriginCounts,
    submissions: Option<&RegistryCounts>,
    skipped: &[String],
    total_bytes: u64,
    data: &Path,
) {
    let version_count: usize = catalog.apps.iter().map(|a| a.versions.len()).sum();
    let restamps = catalog
        .apps
        .iter()
        .flat_map(|a| &a.versions)
        .filter(|v| v.changed == Some(false))
        .count();
    let superseded = catalog
        .apps
        .iter()
        .filter(|a| a.superseded_by.is_some())
        .count();

    println!(
        "\n{} apps · {version_count} versions across {} release(s)",
        catalog.apps.len(),
        catalog.releases.len()
    );
    if !skipped.is_empty() {
        println!("skipped (no usable apps): {}", skipped.join(", "));
    }
    println!("{restamps} version(s) are re-stamps with identical code");
    if superseded > 0 {
        // Not installable: another app owns the folder they would be written to.
        println!("{superseded} app(s) superseded by another owning the same folder");
    }
    println!(
        "{} version(s) built by Kira, {} republished from upstream",
        counts.kira, counts.upstream
    );
    if counts.rejected > 0 {
        println!(
            "{} of Kira's builds were refused as not matching upstream's app",
            counts.rejected
        );
    }
    if let Some(registry) = submissions {
        println!(
            "{} submitted app(s): {} version(s) published, {} with no stored artifact",
            registry.manifests, registry.versions, registry.missing
        );
        if registry.retired > 0 {
            println!(
                "{} submitted version(s) withdrawn, listed but never offered",
                registry.retired
            );
        }
    }
    if counts.diverged > 0 {
        // Expected until the SDK carries the path-independence fix: Kira's build
        // is byte-for-byte different from upstream's for the same source.
        println!(
            "{} of Kira's builds differ byte-for-byte from upstream's",
            counts.diverged
        );
    }
    #[allow(clippy::cast_precision_loss)]
    let mib = total_bytes as f64 / 1024.0 / 1024.0;
    println!("{mib:.2} MiB of binaries -> {}", data.display());
}

/// Annotate each version against the next older one: did the code move?
fn annotate_history(app: &mut App) {
    app.versions
        .sort_by_key(|entry| std::cmp::Reverse(entry.version));
    for index in 0..app.versions.len() {
        // None, not Some(false): with no predecessor published here it is
        // unknown, which the UI reports differently.
        let older = app.versions.get(index + 1).map(|v| {
            (
                v.payload_sha256.clone(),
                i64::try_from(v.size).unwrap_or(i64::MAX),
                v.origin,
            )
        });
        let current_size = i64::try_from(app.versions[index].size).unwrap_or(i64::MAX);
        let entry = &mut app.versions[index];
        match older {
            // Comparing across builders says nothing about whether the code
            // changed -- two builds of one source differ by embedded paths alone.
            // Unknown beats a false claim, and this is what keeps the switch to
            // Kira-built binaries from reading as "code changed" on every app.
            Some((_, _, older_origin)) if older_origin != entry.origin => {
                entry.changed = None;
                entry.delta_bytes = None;
            }
            Some((older_hash, older_size, _)) => {
                entry.changed = Some(entry.payload_sha256 != older_hash);
                entry.delta_bytes = Some(current_size - older_size);
            }
            None => {
                entry.changed = None;
                entry.delta_bytes = None;
            }
        }
    }
    // Present-tense metadata tracks the newest build.
    if let Some(latest) = app.versions.first() {
        app.folder = latest.folder.clone();
    }
}

fn write_png(path: &Path, image: &icon::Rgba) -> Result<()> {
    let file = fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(&image.pixels)
        .context("encoding PNG")?;
    Ok(())
}

/// Millisecond-precision RFC 3339, matching what the previous build emitted.
fn now_rfc3339_millis() -> String {
    let now = jiff::Timestamp::now();
    jiff::fmt::strtime::format(
        "%Y-%m-%dT%H:%M:%S%.3fZ",
        &now.to_zoned(jiff::tz::TimeZone::UTC),
    )
    .unwrap_or_else(|_| now.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_core::uapp::{CRC_LEN, HEADER_LEN, NORMAL_ICON_LEN, SMALL_ICON_LEN};

    const TOOLCHAIN: &str = "sha256:0000test";
    const TIDE_CLOCK: AppId = AppId::new(0xA7C3_1F0E_9B48_2D65);
    const ALARM: AppId = AppId::new(0xA19C_2A7E_4F8B_6D31);

    /// A valid `.uapp`, so these tests need no vendor binaries.
    ///
    /// Icons are left zero-filled, which real Glance builds do too, so nothing
    /// here depends on the PNG encoder.
    fn uapp(app_id: AppId, name: &str, version: Version, filler: u8) -> Vec<u8> {
        const SERVICE: usize = 64;
        let total = HEADER_LEN + NORMAL_ICON_LEN + SMALL_ICON_LEN + SERVICE + CRC_LEN;
        let mut bytes = vec![0u8; total];
        bytes[..8].copy_from_slice(&app_id.get().to_le_bytes());
        bytes[8..12].copy_from_slice(&version.packed().to_le_bytes());
        bytes[12..16].copy_from_slice(&Version::new(0, 0, 3).packed().to_le_bytes());
        bytes[16..20].copy_from_slice(&(SERVICE as u32).to_le_bytes());
        // Bits 0-1 are the type; 1 is Utility.
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        let name = name.as_bytes();
        let len = name.len().min(15);
        bytes[24..24 + len].copy_from_slice(&name[..len]);
        bytes[40..44].copy_from_slice(&(NORMAL_ICON_LEN as u32).to_le_bytes());
        bytes[44..48].copy_from_slice(&(SMALL_ICON_LEN as u32).to_le_bytes());
        let service_at = HEADER_LEN + NORMAL_ICON_LEN + SMALL_ICON_LEN;
        bytes[service_at..service_at + SERVICE].fill(filler);

        // The footer covers everything before it, and the parser already reports
        // what it would expect there -- so stamp that rather than carrying a
        // second CRC implementation into the tests.
        let crc = Uapp::parse(&bytes)
            .expect("a synthetic .uapp parses")
            .verify_crc()
            .computed;
        bytes[total - CRC_LEN..].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    const MANIFEST: &str = r#"
app_id = "A7C31F0E9B482D65"
source = "https://github.com/someone/una-tide-clock"
folder = "TideClock"
licence = "MIT"
maintainer = "someone"

[[versions]]
version = "1.0.0"
rev = "3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d"
sdk_rev = "apps-v1.3.0"
"#;

    /// A workspace holding one upstream release and one submission manifest.
    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }

    impl Fixture {
        fn new(case: &str, manifest: &str) -> Self {
            let root = std::env::temp_dir().join(format!("kira-build-{case}"));
            fs::remove_dir_all(&root).ok();

            let release = root.join("src/apps-v1.3.0/Alarm");
            fs::create_dir_all(&release).unwrap();
            fs::write(
                release.join("Alarm_1.3.0.uapp"),
                uapp(ALARM, "Alarm", Version::new(1, 3, 0), 0x11),
            )
            .unwrap();

            fs::create_dir_all(root.join("registry")).unwrap();
            fs::write(root.join("registry/tide-clock.toml"), manifest).unwrap();
            fs::create_dir_all(root.join("store")).unwrap();

            Self { root }
        }

        /// Put a built binary in the store under the name its recipe gives it,
        /// which is how the catalogue build finds it.
        fn store(&self, version: Version, bytes: &[u8]) {
            let manifest = registry::load_dir(&self.root.join("registry")).unwrap()[0].clone();
            let entry = manifest
                .newest_first()
                .into_iter()
                .find(|e| e.version == version)
                .expect("the manifest lists this version");
            let name = manifest
                .recipe_for(&entry, TOOLCHAIN)
                .artifact_name(&manifest.folder);
            fs::write(self.root.join("store").join(name), bytes).unwrap();
        }

        fn build(&self) -> Result<Catalog> {
            run(&Args {
                src: self.root.join("src"),
                out: self.root.join("site"),
                releases: None,
                repo: None,
                tag: None,
                built: Some(self.root.join("store")),
                toolchain: Some(TOOLCHAIN.to_owned()),
                registry: Some(self.root.join("registry")),
            })?;
            let text = fs::read_to_string(self.root.join("site/data/catalog.json"))?;
            Ok(serde_json::from_str(&text)?)
        }
    }

    #[test]
    fn a_submitted_app_is_published_beside_the_sdk_apps() {
        let fixture = Fixture::new("submitted", MANIFEST);
        let bytes = uapp(TIDE_CLOCK, "Tide Clock", Version::new(1, 0, 0), 0x22);
        fixture.store(Version::new(1, 0, 0), &bytes);

        let catalog = fixture.build().unwrap();
        assert_eq!(catalog.schema, SCHEMA);
        let app = catalog
            .apps
            .iter()
            .find(|a| a.app_id == TIDE_CLOCK)
            .expect("the submission is in the catalogue");

        // In the same list as the SDK's own app, not a section of its own.
        assert!(catalog.apps.iter().any(|a| a.app_id == ALARM));

        let publisher = app
            .publisher
            .as_ref()
            .expect("a submission has a publisher");
        assert_eq!(publisher.repo, "https://github.com/someone/una-tide-clock");
        assert_eq!(publisher.maintainer, "someone");
        assert_eq!(app.retired, None);

        let version = app.latest();
        assert_eq!(version.origin, Origin::Kira);
        assert_eq!(version.sha256, sha256_hex(&bytes));
        assert_eq!(version.file, "Tide_Clock_1.0.0.uapp");
        assert_eq!(
            version.download,
            "apps/registry/tide-clock/TideClock/Tide_Clock_1.0.0.uapp"
        );
        // No vendor binary exists for a submission, so there is nothing to
        // compare against -- unknown, not a claim either way.
        assert_eq!(version.upstream_sha256, None);
        assert_eq!(version.matches_upstream, None);

        let built = version.built_from.as_ref().expect("Kira built it");
        assert_eq!(
            built.app_source,
            "git:https://github.com/someone/una-tide-clock\
             @3f9a1c8e5d2b7046af13c9e8b25d704a6f1c8e3d:."
        );

        // And the bytes are actually served, not merely described.
        let served = fixture.root.join("site/data").join(&version.download);
        assert_eq!(fs::read(served).unwrap(), bytes);
    }

    #[test]
    fn a_version_with_nothing_in_the_store_is_left_out() {
        // There is no vendor binary to fall back on, so publishing an entry for
        // it would name a download that does not exist.
        let fixture = Fixture::new("unbuilt", MANIFEST);
        let catalog = fixture.build().unwrap();
        assert!(!catalog.apps.iter().any(|a| a.app_id == TIDE_CLOCK));
        assert!(catalog.apps.iter().any(|a| a.app_id == ALARM));
    }

    #[test]
    fn a_withdrawn_submission_stays_listed_with_its_reason() {
        // Before the first [[versions]] table, so it withdraws the app itself
        // rather than one of its builds.
        let text = MANIFEST.replace(
            "maintainer = \"someone\"",
            "maintainer = \"someone\"\n\
             retired = \"the sensor it reads was removed in firmware 2.0\"",
        );
        let fixture = Fixture::new("withdrawn", &text);
        fixture.store(
            Version::new(1, 0, 0),
            &uapp(TIDE_CLOCK, "Tide Clock", Version::new(1, 0, 0), 0x22),
        );

        let catalog = fixture.build().unwrap();
        let app = catalog
            .apps
            .iter()
            .find(|a| a.app_id == TIDE_CLOCK)
            .expect("a withdrawn app keeps its listing and its binaries");
        assert_eq!(
            app.retired.as_deref(),
            Some("the sensor it reads was removed in firmware 2.0")
        );
    }

    #[test]
    fn one_withdrawn_version_is_marked_without_taking_the_app_down() {
        let text = MANIFEST.replace(
            "sdk_rev = \"apps-v1.3.0\"\n",
            "sdk_rev = \"apps-v1.3.0\"\nretired = \"writes a corrupt .fit on long runs\"\n\n\
             [[versions]]\nversion = \"1.1.0\"\n\
             rev = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\
             sdk_rev = \"apps-v1.3.0\"\n",
        );
        let fixture = Fixture::new("one-version", &text);
        for version in [Version::new(1, 0, 0), Version::new(1, 1, 0)] {
            fixture.store(
                version,
                &uapp(TIDE_CLOCK, "Tide Clock", version, version.patch()),
            );
        }

        let catalog = fixture.build().unwrap();
        let app = catalog
            .apps
            .iter()
            .find(|a| a.app_id == TIDE_CLOCK)
            .unwrap();
        assert_eq!(app.retired, None, "the app itself is still on offer");
        assert_eq!(app.versions.len(), 2);
        assert_eq!(app.latest().version, Version::new(1, 1, 0));
        assert_eq!(app.latest().retired, None);
        assert_eq!(
            app.find(Version::new(1, 0, 0)).unwrap().retired.as_deref(),
            Some("writes a corrupt .fit on long runs")
        );
    }

    #[test]
    fn a_submission_colliding_with_an_sdk_app_fails_the_build() {
        // The case this guards is upstream shipping the collision *later*: the
        // manifest was fine when it was accepted, and nothing else would notice.
        for (field, replacement, says) in [
            (
                "app_id = \"A7C31F0E9B482D65\"",
                "app_id = \"A19C2A7E4F8B6D31\"",
                "already belongs to Alarm",
            ),
            (
                "folder = \"TideClock\"",
                "folder = \"alarm\"",
                "already used by Alarm",
            ),
        ] {
            let fixture = Fixture::new("collision", &MANIFEST.replace(field, replacement));
            let err = fixture.build().unwrap_err().to_string();
            assert!(err.contains(says), "{err}");
            assert!(err.contains("tide-clock"), "{err}");
        }
    }

    #[test]
    fn a_malformed_manifest_is_never_published_whatever_route_it_took() {
        // The page links a submission's source, so "https and nothing else" has
        // to hold when it is published, not only when it was reviewed.
        let text = MANIFEST.replace(
            "https://github.com/someone/una-tide-clock",
            "javascript:alert(1)",
        );
        let fixture = Fixture::new("malformed", &text);
        let err = fixture.build().unwrap_err().to_string();
        assert!(err.contains("must be an https URL"), "{err}");
    }

    #[test]
    fn a_stored_artifact_that_disagrees_with_its_manifest_is_refused() {
        // Kira built it from a pinned recipe and verified it then, so this is a
        // broken store rather than history to work around.
        let fixture = Fixture::new("mismatch", MANIFEST);
        fixture.store(
            Version::new(1, 0, 0),
            &uapp(ALARM, "Tide Clock", Version::new(1, 0, 0), 0x22),
        );
        let err = fixture.build().unwrap_err().to_string();
        assert!(err.contains("A19C2A7E4F8B6D31"), "{err}");
    }

    #[test]
    fn a_device_file_name_is_path_safe() {
        assert_eq!(
            device_file_name("Tide Clock", Version::new(1, 0, 0)),
            "Tide_Clock_1.0.0.uapp"
        );
        // A real display name: the watch does not care what the file is called,
        // but a path separator in one would be a different file entirely.
        let odd = device_file_name("AVG / R HR", Version::new(1, 3, 0));
        assert!(!odd.contains('/'), "{odd}");
        assert!(!odd.contains(' '), "{odd}");
        assert_eq!(
            device_file_name("", Version::new(0, 1, 0)),
            "app_0.1.0.uapp"
        );
    }
}
