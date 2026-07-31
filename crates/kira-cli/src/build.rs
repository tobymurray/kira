//! Building the published catalogue from unzipped releases.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sha256_hex;
use anyhow::{Context, Result, bail};
use kira_core::catalog::{
    App, BuiltFrom, Catalog, Origin, Release, ReleaseOrder, SCHEMA, Source, VersionEntry,
    partition_unique, sort_newest_first,
};
use kira_core::icon;
use kira_core::uapp::{AppId, Uapp, Version};

use crate::build_app::flags_id;
use crate::recipe::Recipe;
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
        });

        // Icons come from the newest version that has any. A declared length
        // does not mean there are pixels: Glance apps built with icons off
        // carry a zero-filled field of the full size.
        for (slot, field, suffix) in [
            (&mut entry.icon, &normal_icon, ""),
            (&mut entry.icon_small, &small_icon, "@30"),
        ] {
            if slot.is_some() || field.is_empty() || icon::is_blank(field) {
                continue;
            }
            let decoded = icon::decode(field)?;
            let rel = format!("icons/{}{suffix}.png", header.app_id);
            write_png(&data.join(&rel), &decoded)?;
            *slot = Some(rel);
        }
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

    let mut apps: Vec<App> = apps.into_values().collect();
    for app in &mut apps {
        annotate_history(app);
    }
    // Case-insensitive, then exact, so the order is stable across machines.
    // JavaScript's localeCompare depends on the host locale, which made the
    // previous implementation's ordering environment-dependent.
    apps.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    let version_count: usize = apps.iter().map(|a| a.versions.len()).sum();
    let restamps: usize = apps
        .iter()
        .flat_map(|a| &a.versions)
        .filter(|v| v.changed == Some(false))
        .count();

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

    println!(
        "\n{} apps · {version_count} versions across {} release(s)",
        catalog.apps.len(),
        catalog.releases.len()
    );
    if !skipped.is_empty() {
        println!("skipped (no usable apps): {}", skipped.join(", "));
    }
    println!("{restamps} version(s) are re-stamps with identical code");
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
    Ok(())
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
