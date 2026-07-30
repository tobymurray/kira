//! Building one app from source, and verifying what came out.
//!
//! Runs `cmake` and the build tool directly, so this is meant to be invoked
//! inside the pinned toolchain container. Everything it decides — which project
//! directory, which flags, whether the result is acceptable — lives here rather
//! than in workflow YAML, so it can be reasoned about and tested.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use kira_core::uapp::{AppId, AppType, Uapp, Version};

use crate::recipe::Recipe;

/// What an app's `CMakeLists.txt` declares about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declared {
    pub app_id: AppId,
    pub app_type: AppType,
    pub name: String,
}

/// Read `set(VAR "value")` assignments from a `CMake` script.
///
/// Deliberately simplistic: enough for the flat `set(APP_ID "...")` declarations
/// every app uses, and it ignores anything it does not understand rather than
/// guessing.
fn cmake_scalars(text: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("set(") else {
            continue;
        };
        let Some(inner) = rest.strip_suffix(')') else {
            continue;
        };
        let mut parts = inner.splitn(2, char::is_whitespace);
        let Some(name) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        let value = value.trim().trim_matches('"').to_owned();
        if !value.is_empty() {
            found.insert(name.to_owned(), value);
        }
    }
    found
}

/// The single `*-CMake` project directory of an app.
///
/// Glance apps keep theirs under `Software/App`, everything else under
/// `Software/Apps`, so it is discovered. More than one is refused rather than
/// picked from, since the choice would be arbitrary.
pub(crate) fn find_project(app_root: &Path) -> Result<PathBuf> {
    let software = app_root.join("Software");
    ensure!(
        software.is_dir(),
        "{} has no Software directory: not a UNA app",
        app_root.display()
    );

    let mut found = Vec::new();
    for entry in fs::read_dir(&software)?.filter_map(Result::ok) {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for inner in fs::read_dir(entry.path())?.filter_map(Result::ok) {
            let path = inner.path();
            if path.is_dir()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("-CMake"))
                && path.join("CMakeLists.txt").is_file()
            {
                found.push(path);
            }
        }
    }
    found.sort();

    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!(
            "no <name>-CMake directory with a CMakeLists.txt under {}",
            software.display()
        ),
        many => bail!(
            "{} CMake projects under {}, expected 1: {}",
            many.len(),
            software.display(),
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Read what the project declares about itself.
pub(crate) fn read_declared(project: &Path) -> Result<Declared> {
    let path = project.join("CMakeLists.txt");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let scalars = cmake_scalars(&text);

    let get = |key: &str| -> Result<&String> {
        scalars
            .get(key)
            .with_context(|| format!("{} declares no {key}", path.display()))
    };

    let app_id: AppId = get("APP_ID")?
        .parse()
        .with_context(|| format!("{}: APP_ID is not 16 hex digits", path.display()))?;
    let declared_type = get("APP_TYPE")?;
    let app_type = match declared_type.as_str() {
        "Activity" => AppType::Activity,
        "Utility" => AppType::Utility,
        "Glance" => AppType::Glance,
        "Clockface" => AppType::Clockface,
        other => bail!("{}: unknown APP_TYPE {other}", path.display()),
    };
    // APP_USER_NAME is what reaches the binary when present; APP_NAME otherwise.
    let name = scalars
        .get("APP_USER_NAME")
        .or_else(|| scalars.get("APP_NAME"))
        .with_context(|| format!("{} declares no APP_NAME", path.display()))?
        .clone();

    Ok(Declared {
        app_id,
        app_type,
        name,
    })
}

/// Canonical description of the flags Kira adds, for the recipe key.
///
/// Describes them rather than quoting them: the real arguments contain absolute
/// paths, which would make the cache key depend on the build directory and defeat
/// the whole point.
#[must_use]
pub(crate) fn flags_id() -> String {
    "macro-prefix-map:sdk=/una-sdk,app=/una-app".to_owned()
}

/// The actual compiler arguments for a given pair of trees.
///
/// Kira supplies these itself so its builds are path-independent whether or not
/// the SDK carries the equivalent fix yet.
fn flag_args(sdk: &Path, app: &Path) -> String {
    format!(
        "-fmacro-prefix-map={}=/una-sdk -fmacro-prefix-map={}=/una-app",
        sdk.display(),
        app.display()
    )
}

/// Check a freshly built binary against the source it came from.
///
/// This is the gate that makes "Kira built this" mean something: a binary that
/// disagrees with its own `CMakeLists.txt` about its identity, or with the
/// requested version, or that fails its own integrity check, is not publishable.
fn verify(
    bytes: &[u8],
    path: &Path,
    declared: &Declared,
    version: Version,
) -> Result<kira_core::uapp::Header> {
    let parsed = Uapp::parse(bytes).with_context(|| format!("parsing {}", path.display()))?;
    let header = parsed.header().clone();

    let crc = parsed.verify_crc();
    ensure!(
        crc.is_valid(),
        "{}: CRC mismatch (stored {:#010x}, computed {:#010x})",
        path.display(),
        crc.stored,
        crc.computed
    );
    ensure!(
        header.app_id == declared.app_id,
        "built AppID {} does not match {} declared in CMakeLists.txt",
        header.app_id,
        declared.app_id
    );
    ensure!(
        header.app_type() == declared.app_type,
        "built type {} does not match {} declared in CMakeLists.txt",
        header.app_type(),
        declared.app_type
    );
    ensure!(
        header.version == version,
        "built version {} does not match the requested {version}",
        header.version
    );
    Ok(header)
}

/// Inputs for one build.
#[derive(Debug)]
pub(crate) struct Args {
    /// The app's source tree, containing `Software/`.
    pub app: PathBuf,
    /// An SDK checkout.
    pub sdk: PathBuf,
    /// Version to stamp into the binary.
    pub version: Version,
    /// Where to write the verified `.uapp`.
    pub out: PathBuf,
    /// `CMake` generator. Output is identical either way; this exists to make that
    /// testable rather than assumed.
    pub generator: String,
    /// Parallel build jobs.
    pub jobs: usize,
    /// Toolchain identifier recorded in the recipe, normally a container digest.
    pub toolchain: String,
    /// Canonical identity of the app's source, recorded in the recipe.
    pub app_source: String,
    /// SDK revision recorded in the recipe.
    pub sdk_rev: String,
}

/// A completed, verified build.
///
/// Returned in full so a future caller -- the submission registry -- can record
/// provenance without rebuilding; the `build-app` subcommand only needs the file
/// and the printed summary.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "provenance fields are for the registry, not the CLI"
)]
pub(crate) struct Built {
    pub declared: Declared,
    pub recipe: Recipe,
    pub artifact: String,
    pub size: usize,
    pub sha256: String,
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("could not start {what}"))?;
    ensure!(status.success(), "{what} failed with {status}");
    Ok(())
}

/// Build one app and verify the result against what its source declares.
///
/// # Errors
/// If the project cannot be located, the build fails, or the resulting binary
/// disagrees with its own source about its identity, version or integrity.
pub(crate) fn run_build(args: &Args) -> Result<Built> {
    let app = fs::canonicalize(&args.app)
        .with_context(|| format!("no such app directory: {}", args.app.display()))?;
    let sdk = fs::canonicalize(&args.sdk)
        .with_context(|| format!("no such SDK directory: {}", args.sdk.display()))?;

    let project = find_project(&app)?;
    let declared = read_declared(&project)?;
    println!(
        "building {} ({}) {} from {}",
        declared.name,
        declared.app_type,
        args.version,
        project.display()
    );

    let build_dir = project.join("build");
    fs::create_dir_all(&build_dir)?;
    let flags = flag_args(&sdk, &app);

    run(
        Command::new("cmake")
            .current_dir(&build_dir)
            .env("UNA_SDK", &sdk)
            .arg("-G")
            .arg(&args.generator)
            .arg(format!("-DBUILD_VERSION={}", args.version))
            .arg(format!("-DCMAKE_C_FLAGS={flags}"))
            .arg(format!("-DCMAKE_CXX_FLAGS={flags}"))
            .arg(".."),
        "cmake configure",
    )?;
    run(
        Command::new("cmake")
            .current_dir(&build_dir)
            .env("UNA_SDK", &sdk)
            .args(["--build", ".", "--parallel"])
            .arg(args.jobs.to_string()),
        "cmake build",
    )?;

    // The merge step writes into the binary directory.
    let mut produced: Vec<PathBuf> = fs::read_dir(&build_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("uapp"))
        })
        .collect();
    produced.sort();
    let [uapp] = produced.as_slice() else {
        bail!(
            "expected exactly one .uapp in {}, found {}",
            build_dir.display(),
            produced.len()
        );
    };

    let bytes = fs::read(uapp)?;
    let header = verify(&bytes, uapp, &declared, args.version)?;

    let recipe = Recipe {
        app_source: args.app_source.clone(),
        sdk_rev: args.sdk_rev.clone(),
        toolchain: args.toolchain.clone(),
        build_version: args.version,
        flags: flags_id(),
    };
    let artifact = recipe.artifact_name(header.app_id);

    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&args.out, &bytes).with_context(|| format!("writing {}", args.out.display()))?;

    let sha256 = crate::sha256_hex(&bytes);
    println!(
        "  ok  {} bytes  id={}  recipe={}  sha256={sha256}",
        bytes.len(),
        header.app_id,
        recipe.key()
    );
    println!("  artifact name: {artifact}");

    Ok(Built {
        declared,
        recipe,
        artifact,
        size: bytes.len(),
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flat_cmake_assignments() {
        let text = r#"
            cmake_minimum_required(VERSION 3.21)
            set(APP_NAME "Alarm")
            set(APP_USER_NAME "Alarm")
            set(APP_TYPE "Utility")
            set(APP_AUTOSTART On)
            set(APP_ID "A19C2A7E4F8B6D31")
        "#;
        let scalars = cmake_scalars(text);
        assert_eq!(scalars.get("APP_ID").unwrap(), "A19C2A7E4F8B6D31");
        assert_eq!(scalars.get("APP_TYPE").unwrap(), "Utility");
        assert_eq!(scalars.get("APP_AUTOSTART").unwrap(), "On");
        // Not a scalar assignment, so it must not appear.
        assert!(!scalars.contains_key("cmake_minimum_required"));
    }

    #[test]
    fn tolerates_unquoted_values_and_odd_spacing() {
        let scalars = cmake_scalars("set(APP_ID A19C2A7E4F8B6D31)\n   set(APP_TYPE   \"Glance\")");
        assert_eq!(scalars.get("APP_ID").unwrap(), "A19C2A7E4F8B6D31");
        assert_eq!(scalars.get("APP_TYPE").unwrap(), "Glance");
    }

    #[test]
    fn ignores_multi_line_and_list_forms_rather_than_guessing() {
        // A multi-line set() is not something this parser claims to handle; it
        // must skip rather than capture a fragment.
        let scalars = cmake_scalars("set(SERVICE_SOURCES\n  a.cpp\n  b.cpp\n)");
        assert!(!scalars.contains_key("SERVICE_SOURCES"));
    }

    #[test]
    fn flags_id_is_path_independent() {
        // The recipe key must not vary with the build directory.
        assert_eq!(flags_id(), flags_id());
        assert!(!flags_id().contains('/') || !flags_id().contains("/work"));
        assert!(!flags_id().contains("tmp"));
    }

    #[test]
    fn flag_args_reference_the_actual_trees() {
        let args = flag_args(Path::new("/a/sdk"), Path::new("/b/app"));
        assert!(args.contains("-fmacro-prefix-map=/a/sdk=/una-sdk"));
        assert!(args.contains("-fmacro-prefix-map=/b/app=/una-app"));
    }

    #[test]
    fn refuses_a_tree_with_no_software_directory() {
        let dir = std::env::temp_dir().join("kira-build-app-test-empty");
        fs::create_dir_all(&dir).unwrap();
        let err = find_project(&dir).unwrap_err().to_string();
        assert!(err.contains("no Software directory"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_more_than_one_cmake_project() {
        let root = std::env::temp_dir().join("kira-build-app-test-ambiguous");
        for name in ["Apps/One-CMake", "Apps/Two-CMake"] {
            let dir = root.join("Software").join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("CMakeLists.txt"), "set(APP_ID \"A\")").unwrap();
        }
        let err = find_project(&root).unwrap_err().to_string();
        assert!(err.contains("expected 1"), "{err}");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn finds_a_glance_project_under_software_app() {
        // Glance apps use Software/App, not Software/Apps.
        let root = std::env::temp_dir().join("kira-build-app-test-glance");
        let dir = root.join("Software/App/GlanceHR-CMake");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("CMakeLists.txt"),
            "set(APP_NAME \"GlanceHR\")\nset(APP_TYPE \"Glance\")\nset(APP_ID \"A1358F7C2E9D4BA6\")",
        )
        .unwrap();

        let project = find_project(&root).unwrap();
        let declared = read_declared(&project).unwrap();
        assert_eq!(declared.app_type, AppType::Glance);
        assert_eq!(declared.app_id.to_string(), "A1358F7C2E9D4BA6");
        assert_eq!(declared.name, "GlanceHR");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_a_malformed_app_id() {
        let root = std::env::temp_dir().join("kira-build-app-test-badid");
        let dir = root.join("Software/Apps/X-CMake");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("CMakeLists.txt"),
            "set(APP_NAME \"X\")\nset(APP_TYPE \"Utility\")\nset(APP_ID \"nope\")",
        )
        .unwrap();
        let err = read_declared(&dir).unwrap_err().to_string();
        assert!(err.contains("16 hex digits"), "{err}");
        fs::remove_dir_all(&root).ok();
    }
}
