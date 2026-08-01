//! Planner check against two real una-apps releases.
//!
//! App versions are stamped from the `apps-v*` tag and applied to every app in
//! the release, so a version bump on its own proves nothing about whether an app
//! changed. This drives the real thing: an older release as "what is installed",
//! a newer one as the catalogue, and asserts the planner separates genuine
//! updates from pure re-stamps.
//!
//! Opt in by pointing at two unzipped releases:
//!
//! ```text
//! KIRA_FIXTURE_OLD=/path/to/apps-v1.2.0 \
//! KIRA_FIXTURE_NEW=/path/to/apps-v1.3.0 \
//! cargo test -p kira-core
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use kira_core::catalog::{App, AppType, Catalog, Origin, Source, VersionEntry, resolve_targets};
use kira_core::plan::{self, Installed, Status};
use kira_core::uapp::Uapp;
use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

struct Fixture {
    folder: String,
    file: String,
    bytes: Vec<u8>,
}

/// Walk `<root>/<Folder>/<one>.uapp`, mirroring the release zip layout.
///
/// Note that cargo runs a test binary with the crate directory as its working
/// directory, so relative fixture paths are resolved against `crates/kira-core`
/// rather than the workspace root. Absolute paths avoid the surprise.
fn collect(root: &Path) -> Vec<Fixture> {
    assert!(
        root.is_dir(),
        "fixture directory not found: {} (an absolute path is usually wanted)",
        root.display()
    );
    let mut found = Vec::new();
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .expect("fixture directory is readable")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort();

    for dir in dirs {
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("app directory is readable")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("uapp"))
            })
            .collect();
        files.sort();

        if let Some(path) = files.first() {
            found.push(Fixture {
                folder: dir
                    .file_name()
                    .expect("directory has a name")
                    .to_string_lossy()
                    .into_owned(),
                file: path
                    .file_name()
                    .expect("file has a name")
                    .to_string_lossy()
                    .into_owned(),
                bytes: fs::read(path).expect("uapp is readable"),
            });
        }
    }
    found
}

fn catalog_from(fixtures: &[Fixture], tag: &str) -> Catalog {
    let apps = fixtures
        .iter()
        .map(|fixture| {
            let uapp = Uapp::parse(&fixture.bytes).expect("fixture parses");
            let header = uapp.header();
            App {
                app_id: header.app_id,
                name: header.name.clone(),
                app_type: header.app_type(),
                folder: fixture.folder.clone(),
                versions: vec![VersionEntry {
                    version: header.version,
                    version_packed: header.version.packed(),
                    tag: tag.to_owned(),
                    folder: fixture.folder.clone(),
                    file: fixture.file.clone(),
                    libc_version: header.libc_version,
                    autostart: header.autostart(),
                    size: fixture.bytes.len(),
                    sha256: sha256_hex(&fixture.bytes),
                    payload_sha256: sha256_hex(uapp.payload()),
                    download: format!("apps/{tag}/{}/{}", fixture.folder, fixture.file),
                    changed: None,
                    delta_bytes: None,
                    origin: Origin::Kira,
                    built_from: None,
                    upstream_sha256: None,
                    matches_upstream: None,
                    retired: None,
                    notes: None,
                }],
                icon: None,
                icon_small: None,
                superseded_by: None,
                publisher: None,
                retired: None,
            }
        })
        .collect();

    Catalog {
        schema: kira_core::catalog::SCHEMA,
        generated: "fixture".into(),
        source: Source { repo: None },
        releases: Vec::new(),
        apps,
    }
}

fn installed_from(fixtures: &[Fixture]) -> Vec<Installed> {
    fixtures
        .iter()
        .map(|fixture| {
            let uapp = Uapp::parse(&fixture.bytes).expect("fixture parses");
            Installed {
                app_id: uapp.header().app_id,
                folder: fixture.folder.clone(),
                file: fixture.file.clone(),
                name: uapp.header().name.clone(),
                version: uapp.header().version,
                size: fixture.bytes.len(),
                extra_uapps: Vec::new(),
                payload_sha256: Some(sha256_hex(uapp.payload())),
                sha256: Some(sha256_hex(&fixture.bytes)),
                crc_valid: Some(uapp.verify_crc().is_valid()),
            }
        })
        .collect()
}

#[test]
fn separates_real_updates_from_release_tag_restamps() {
    let (Ok(old), Ok(new)) = (
        std::env::var("KIRA_FIXTURE_OLD"),
        std::env::var("KIRA_FIXTURE_NEW"),
    ) else {
        eprintln!("skipped: set KIRA_FIXTURE_OLD and KIRA_FIXTURE_NEW to run");
        return;
    };

    let old_fixtures = collect(Path::new(&old));
    let new_fixtures = collect(Path::new(&new));
    assert!(!new_fixtures.is_empty(), "no fixtures found in {new}");

    let catalog = catalog_from(&new_fixtures, "apps-new");
    let installed = installed_from(&old_fixtures);
    let targets = resolve_targets(&catalog, &BTreeMap::new());
    let result = plan::build(&targets, &installed);

    assert!(result.foreign.is_empty(), "no unknown apps expected");
    assert!(
        result.entries.iter().all(|e| e.status == Status::Update),
        "every entry should be an update, not an install"
    );

    let mut restamped: Vec<&str> = result
        .entries
        .iter()
        .filter(|e| e.identical_payload)
        .map(|e| e.app.folder.as_str())
        .collect();
    let mut changed: Vec<&str> = result
        .entries
        .iter()
        .filter(|e| !e.identical_payload)
        .map(|e| e.app.folder.as_str())
        .collect();
    restamped.sort_unstable();
    changed.sort_unstable();

    // Cross-check against the bytes, computed independently of the planner, so
    // this holds for whichever two releases the fixtures point at.
    let old_by_folder: BTreeMap<&str, &Fixture> = old_fixtures
        .iter()
        .map(|f| (f.folder.as_str(), f))
        .collect();
    let mut expect_restamped = Vec::new();
    let mut expect_changed = Vec::new();
    for fixture in &new_fixtures {
        let Some(before) = old_by_folder.get(fixture.folder.as_str()) else {
            continue;
        };
        let payload = |f: &Fixture| sha256_hex(Uapp::parse(&f.bytes).unwrap().payload());
        if payload(fixture) == payload(before) {
            expect_restamped.push(fixture.folder.as_str());
        } else {
            expect_changed.push(fixture.folder.as_str());
        }
    }
    expect_restamped.sort_unstable();
    expect_changed.sort_unstable();

    assert_eq!(restamped, expect_restamped);
    assert_eq!(changed, expect_changed);
    assert!(
        !restamped.is_empty() || !changed.is_empty(),
        "fixtures produced no comparisons"
    );

    // Regression pin for the pair this behaviour was designed against: six of the
    // thirteen apps in apps-v1.3.0 are byte-identical to their 1.2.0 builds.
    let old_version = installed[0].version.to_string();
    let new_version = catalog.apps[0].versions[0].version.to_string();
    if old_version == "1.2.0" && new_version == "1.3.0" {
        assert_eq!(
            restamped,
            [
                "GlanceARHR",
                "GlanceActivity",
                "GlanceBattery",
                "GlanceFloors",
                "GlanceHR",
                "GlanceSteps"
            ]
        );
        assert_eq!(changed.len(), 7);
    }

    // Every Glance in these releases is icon-less, which the catalogue build
    // relies on when deciding not to emit a PNG.
    let glances = catalog
        .apps
        .iter()
        .filter(|a| a.app_type == AppType::Glance)
        .count();
    assert!(glances > 0, "expected some Glance apps in the fixtures");
}
