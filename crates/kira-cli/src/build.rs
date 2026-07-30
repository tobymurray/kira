//! Building the published catalogue from unzipped releases.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sha256_hex;
use anyhow::{Context, Result, bail};
use kira_core::catalog::{
    App, Catalog, Origin, Release, ReleaseOrder, SCHEMA, Source, VersionEntry, partition_unique,
    sort_newest_first,
};
use kira_core::icon;
use kira_core::uapp::{AppId, Uapp};
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
                found.push(Binary {
                    folder,
                    file: file.clone(),
                    bytes: fs::read(&path)
                        .with_context(|| format!("reading {}", path.display()))?,
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

/// A binary with its header parsed and its hashes computed.
struct Parsed {
    binary: Binary,
    header: kira_core::uapp::Header,
    payload_sha256: String,
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
                payload_sha256: sha256_hex(uapp.payload()),
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
        payload_sha256,
        normal_icon,
        small_icon,
    } in partitioned.unique
    {
        let download = format!("apps/{}/{}/{}", release.tag, binary.folder, binary.file);
        let target = data.join(&download);
        fs::create_dir_all(target.parent().expect("download path has a parent"))?;
        fs::write(&target, &binary.bytes)
            .with_context(|| format!("writing {}", target.display()))?;
        *total_bytes += binary.bytes.len() as u64;

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
            size: binary.bytes.len(),
            sha256: sha256_hex(&binary.bytes),
            payload_sha256,
            download,
            // Filled once every version is known.
            changed: None,
            delta_bytes: None,
            // These binaries come straight from an upstream release, so they are
            // upstream's by definition and trivially match themselves. When the
            // pipeline switches to Kira-built binaries this becomes Origin::Kira
            // with a builtFrom, and matchesUpstream stops being a tautology.
            origin: Origin::Upstream,
            upstream_sha256: Some(sha256_hex(&binary.bytes)),
            matches_upstream: Some(true),
            built_from: None,
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

    let mut apps: BTreeMap<AppId, App> = BTreeMap::new();
    let mut releases: Vec<Release> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut total_bytes: u64 = 0;

    for release in &releases_dirs {
        match process_release(release, &data, &mut apps, &mut total_bytes)? {
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
            )
        });
        let current_size = i64::try_from(app.versions[index].size).unwrap_or(i64::MAX);
        let entry = &mut app.versions[index];
        if let Some((older_hash, older_size)) = older {
            entry.changed = Some(entry.payload_sha256 != older_hash);
            entry.delta_bytes = Some(current_size - older_size);
        } else {
            entry.changed = None;
            entry.delta_bytes = None;
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
