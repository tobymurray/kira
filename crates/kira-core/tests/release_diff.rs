//! Planner check against two real una-apps releases.
//!
//! App versions are stamped from the `apps-v*` tag and applied to every app in
//! the release, so a version bump on its own proves nothing about whether an app
//! changed. This drives the real thing: an older release as "what is installed",
//! a newer one as the catalogue, and asserts the planner separates genuine
//! updates from pure re-stamps.
//!
//! A release is **not** a superset of the one before it, and everything here is
//! keyed on [`AppId`] because of it. apps-v1.4.0 dropped `HRMonitor`, added
//! `Stopwatch`, `Timer` and `Walk`, and renamed `Cycling`, `Hiking` and `Running`
//! to `Bike`, `Hike` and `Run` while keeping both their ids and their folders. So
//! which apps are foreign and which are first-time installs is derived from the
//! fixtures rather than assumed: what is asserted is that the planner agrees with
//! the bytes, not that upstream never adds, drops or renames an app.
//!
//! Opt in by pointing at two unzipped releases:
//!
//! ```text
//! KIRA_FIXTURE_OLD=/path/to/apps-v1.3.0 \
//! KIRA_FIXTURE_NEW=/path/to/apps-v1.4.0 \
//! cargo test -p kira-core
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use kira_core::catalog::{
    App, AppType, Catalog, Origin, Source, Variant, VersionEntry, resolve_targets,
};
use kira_core::plan::{self, Installed, Status};
use kira_core::uapp::{AppId, Uapp, Version};
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

fn app_id(fixture: &Fixture) -> AppId {
    Uapp::parse(&fixture.bytes)
        .expect("fixture parses")
        .header()
        .app_id
}

/// The payload hash, computed here rather than read back from the planner, so the
/// cross-check below is independent of the code it is checking.
fn payload_hash(fixture: &Fixture) -> String {
    sha256_hex(
        Uapp::parse(&fixture.bytes)
            .expect("fixture parses")
            .payload(),
    )
}

/// Index a release by the only identity an app really has.
///
/// `which` names the fixture set in the failure, because a duplicate id is a
/// property of the release rather than of the test: two of the apps-v0.1.9-rc
/// releases ship different apps under one `AppId`, which the catalogue build drops
/// both sides of. This comparison has no way to key on that, so it says so
/// instead of silently keeping whichever one it saw last.
fn by_id<'a>(fixtures: &'a [Fixture], which: &str) -> BTreeMap<AppId, &'a Fixture> {
    let map: BTreeMap<AppId, &Fixture> = fixtures.iter().map(|f| (app_id(f), f)).collect();
    assert_eq!(
        map.len(),
        fixtures.len(),
        "the {which} release ships two apps under one AppID, which this comparison cannot key on"
    );
    map
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
                    prerelease: None,
                    supersedes_sha256: Vec::new(),
                    tag: tag.to_owned(),
                    folder: fixture.folder.clone(),
                    file: fixture.file.clone(),
                    libc_version: header.libc_version,
                    autostart: header.autostart(),
                    variant: Variant::of(&uapp),
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
                config: None,
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
                variant: Variant::of(&uapp),
            }
        })
        .collect()
}

/// Folders of the apps the newer release introduces, sorted.
fn installs(result: &plan::Plan) -> Vec<&str> {
    let mut folders: Vec<&str> = result
        .entries
        .iter()
        .filter(|e| e.status == Status::Install)
        .map(|e| e.app.folder.as_str())
        .collect();
    folders.sort_unstable();
    folders
}

/// How one release pair came out, as the planner reported it.
struct Split<'a> {
    restamped: &'a [&'a str],
    changed: &'a [&'a str],
    foreign: &'a [&'a str],
    installs: &'a [&'a str],
}

/// The assertions above hold for any two releases; these hold only for the two
/// pairs whose contents are known, and exist so a change in the planner cannot
/// quietly agree with itself.
fn pin_known_pairs(old_version: &str, new_version: &str, split: &Split) {
    // The pair this behaviour was designed against: six of the thirteen apps in
    // apps-v1.3.0 are byte-identical to their 1.2.0 builds, and 1.3.0 carries
    // every app 1.2.0 had.
    if old_version == "1.2.0" && new_version == "1.3.0" {
        assert_eq!(
            split.restamped,
            [
                "GlanceARHR",
                "GlanceActivity",
                "GlanceBattery",
                "GlanceFloors",
                "GlanceHR",
                "GlanceSteps"
            ]
        );
        assert_eq!(split.changed.len(), 7);
        assert!(split.foreign.is_empty());
        assert!(split.installs.is_empty());
    }

    // The pair CI runs today. apps-v1.4.0 is the first release that is not a
    // superset of its predecessor: it drops HRMonitor, adds Stopwatch, Timer and
    // Walk, and re-stamps nothing -- all twelve apps it carries over also changed.
    if old_version == "1.3.0" && new_version == "1.4.0" {
        assert_eq!(split.foreign, ["HRMonitor"]);
        assert_eq!(split.installs, ["Stopwatch", "Timer", "Walking"]);
        assert!(split.restamped.is_empty());
        assert_eq!(split.changed.len(), 12);
    }
}

/// Decode an alias descriptor straight from the bytes, the way
/// `make_variant.py` packs one: `struct.pack("<IQIBI11s")` at a fixed
/// 48 + 3600 + 900. Deliberately independent of `Uapp::variant`, so this checks
/// the reader against the packer rather than against itself.
fn descriptor_of(bytes: &[u8]) -> (u32, AppId, u32, u8, usize, &str) {
    let at = 48 + 3600 + 900;
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let size = u32_at(at + 17) as usize;
    (
        u32_at(at),
        AppId::new(u64::from_le_bytes(
            bytes[at + 4..at + 12].try_into().unwrap(),
        )),
        u32_at(at + 12),
        bytes[at + 16],
        size,
        std::str::from_utf8(&bytes[at + 32..at + 32 + size]).expect("the config is text"),
    )
}

#[test]
fn a_variant_alias_reads_as_one_against_the_shipped_bytes() {
    let Ok(new) = std::env::var("KIRA_FIXTURE_NEW") else {
        eprintln!("skipped: set KIRA_FIXTURE_NEW to run");
        return;
    };
    let fixtures = collect(Path::new(&new));
    let by_id = by_id(&fixtures, "newer");

    let aliases: Vec<&Fixture> = fixtures
        .iter()
        .filter(|f| {
            Uapp::parse(&f.bytes)
                .expect("fixture parses")
                .header()
                .is_variant_alias()
        })
        .collect();

    for fixture in &aliases {
        let uapp = Uapp::parse(&fixture.bytes).expect("fixture parses");
        let alias = uapp
            .variant()
            .expect("the flag is set")
            .unwrap_or_else(|e| panic!("{}: descriptor unreadable -- {e}", fixture.folder));

        let (payload_version, target, min_target, origin, config_size, config) =
            descriptor_of(&fixture.bytes);
        assert_eq!(payload_version, 1, "{}", fixture.folder);
        assert_eq!(alias.target, target);
        assert_eq!(
            alias.min_target_version.map_or(0, Version::packed),
            min_target
        );
        assert_eq!(
            alias.origin.to_string(),
            ["shipped", "user"][origin as usize]
        );
        assert_eq!(alias.config, config);
        assert_eq!(alias.config.len(), config_size);

        // An alias carries no code and never claims its target's identity.
        assert_eq!(uapp.header().service_len, 0);
        assert_eq!(uapp.header().libc_version.packed(), 0);
        assert_ne!(alias.target, uapp.header().app_id);
        // Its target ships in the same release, which is what makes the
        // catalogue link resolvable at all.
        assert!(
            by_id.contains_key(&alias.target),
            "{} targets {}, which is not in this release",
            fixture.folder,
            alias.target
        );
    }

    // Pinned for the one release whose contents are known. apps-v1.4.0 ships
    // exactly one variant: Walk, on the Hiking binary, with a 1.4.0 floor.
    let version = Uapp::parse(&fixtures[0].bytes)
        .expect("fixture parses")
        .header()
        .version;
    if version.to_string() == "1.4.0" {
        let folders: Vec<&str> = aliases.iter().map(|f| f.folder.as_str()).collect();
        assert_eq!(folders, ["Walking"]);
        let uapp = Uapp::parse(&aliases[0].bytes).expect("fixture parses");
        let alias = uapp.variant().unwrap().unwrap();
        assert_eq!(uapp.header().name, "Walk");
        assert_eq!(by_id[&alias.target].folder, "Hiking");
        assert_eq!(
            alias.min_target_version.map(|v| v.to_string()).as_deref(),
            Some("1.4.0")
        );
        assert_eq!(alias.origin.to_string(), "shipped");
    }
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

    let old_by_id = by_id(&old_fixtures, "older");
    let new_by_id = by_id(&new_fixtures, "newer");

    // An app the older release carried and the newer one does not is on the watch
    // and in no catalogue entry, which is exactly what `foreign` is for: name it
    // and leave it alone. Upstream dropped HRMonitor in apps-v1.4.0, so this is a
    // real state rather than a hypothetical one.
    let mut foreign: Vec<&str> = result.foreign.iter().map(|i| i.folder.as_str()).collect();
    let mut expect_foreign: Vec<&str> = old_fixtures
        .iter()
        .filter(|f| !new_by_id.contains_key(&app_id(f)))
        .map(|f| f.folder.as_str())
        .collect();
    foreign.sort_unstable();
    expect_foreign.sort_unstable();
    assert_eq!(
        foreign, expect_foreign,
        "apps only in the older release should be reported foreign, and nothing else"
    );

    // Carried over by both releases is an update; new in the newer release is an
    // install. Neither Current nor NewerOnWatch is reachable from two different
    // releases in the right order, so anything else means the fixtures are the
    // same release or the wrong way round.
    for entry in &result.entries {
        let expected = if old_by_id.contains_key(&entry.app.app_id) {
            Status::Update
        } else {
            Status::Install
        };
        assert_eq!(
            entry.status, expected,
            "unexpected status for {} ({})",
            entry.app.folder, entry.app.app_id
        );
    }

    // Of the apps both releases carry, which moved and which are pure re-stamps.
    // Restricted to updates because an app appearing for the first time has no
    // predecessor to be identical to.
    let mut restamped: Vec<&str> = result
        .entries
        .iter()
        .filter(|e| e.status == Status::Update && e.identical_payload)
        .map(|e| e.app.folder.as_str())
        .collect();
    let mut changed: Vec<&str> = result
        .entries
        .iter()
        .filter(|e| e.status == Status::Update && !e.identical_payload)
        .map(|e| e.app.folder.as_str())
        .collect();
    restamped.sort_unstable();
    changed.sort_unstable();

    // Cross-check against the bytes, computed independently of the planner, so
    // this holds for whichever two releases the fixtures point at.
    let mut expect_restamped = Vec::new();
    let mut expect_changed = Vec::new();
    for fixture in &new_fixtures {
        let Some(before) = old_by_id.get(&app_id(fixture)) else {
            continue;
        };
        if payload_hash(fixture) == payload_hash(before) {
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

    pin_known_pairs(
        &installed[0].version.to_string(),
        &catalog.apps[0].versions[0].version.to_string(),
        &Split {
            restamped: &restamped,
            changed: &changed,
            foreign: &foreign,
            installs: &installs(&result),
        },
    );

    // Every Glance in these releases is icon-less, which the catalogue build
    // relies on when deciding not to emit a PNG.
    let glances = catalog
        .apps
        .iter()
        .filter(|a| a.app_type == AppType::Glance)
        .count();
    assert!(glances > 0, "expected some Glance apps in the fixtures");
}
