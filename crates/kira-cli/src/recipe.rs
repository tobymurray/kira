//! Build recipes, and deciding what has to be built.
//!
//! A `.uapp` is a function of more than its source: the SDK it links against,
//! the toolchain that compiled it, the version string stamped into it, and the
//! flags used. Two of those are outside this project's control and change
//! independently, so cached artifacts are keyed by all of them rather than by
//! app and version alone. A toolchain bump then yields new artifacts instead of
//! quietly mixing incompatible ones.
//!
//! Lives in the CLI, not in `kira-core`: hashing recipes is a build-side concern
//! and the browser has no use for it.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use kira_core::uapp::{AppId, Version};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::build_app::{find_project, flags_id, read_declared};

/// Bump when the meaning of a recipe changes, to deliberately invalidate every
/// cached artifact rather than silently reusing one built to older rules.
const RECIPE_SCHEME: &str = "kira-recipe-1";

/// Everything that determines the bytes of a built `.uapp`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recipe {
    /// Canonical identity of the app's source, e.g.
    /// `sdk:apps-v1.3.0:Examples/Apps/Alarm` for an app that ships in the SDK,
    /// or `git:https://github.com/owner/repo@<sha>:.` for a submission.
    pub app_source: String,
    /// The SDK revision the app is compiled against.
    pub sdk_rev: String,
    /// Toolchain container image, pinned by digest.
    pub toolchain: String,
    /// The version string stamped into the binary. Part of the recipe because it
    /// is compiled in, so it changes the output.
    pub build_version: Version,
    /// Extra compile flags Kira supplies, canonicalised.
    ///
    /// Kira passes its own `-fmacro-prefix-map` so that builds are path
    /// independent whether or not the SDK carries that fix yet.
    pub flags: String,
}

impl Recipe {
    /// A stable, canonical serialisation. Field order is fixed and each value is
    /// on its own line, so the digest cannot be perturbed by formatting.
    fn canonical(&self) -> String {
        let mut out = String::new();
        for (field, value) in [
            ("scheme", RECIPE_SCHEME),
            ("app_source", &self.app_source),
            ("sdk_rev", &self.sdk_rev),
            ("toolchain", &self.toolchain),
            ("build_version", &self.build_version.to_string()),
            ("flags", &self.flags),
        ] {
            let _ = writeln!(out, "{field}={value}");
        }
        out
    }

    /// Short digest of the recipe, used in artifact names.
    ///
    /// 16 hex characters, i.e. 64 bits. Collisions are not adversarially
    /// interesting here — a wrong guess yields a build that then fails
    /// verification against its declared `AppID` and version.
    #[must_use]
    pub(crate) fn key(&self) -> String {
        let digest = Sha256::digest(self.canonical().as_bytes());
        digest.iter().take(8).fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
    }

    /// Name of the cached artifact for this recipe.
    ///
    /// Flat, because release assets have no directories. Leads with a
    /// human-readable label so a listing can be skimmed; uniqueness comes from
    /// the recipe key, not the label, so two apps sharing a folder name across
    /// releases still get distinct artifacts.
    #[must_use]
    pub(crate) fn artifact_name(&self, label: &str) -> String {
        format!(
            "{}-{}-{}.uapp",
            sanitise(label),
            self.build_version,
            self.key()
        )
    }
}

/// Keep a label safe for a flat asset name.
///
/// Folder names are already path-safe, but a submission's could be anything.
fn sanitise(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "app".to_owned()
    } else {
        cleaned
    }
}

/// One app that the catalogue wants at a particular version.
#[derive(Debug, Clone)]
pub(crate) struct Wanted {
    pub app_id: AppId,
    /// Folder the app occupies on the watch, for logging.
    pub folder: String,
    pub recipe: Recipe,
}

/// What to do about a wanted artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// Already in the cache under this asset name.
    Fetch(String),
    /// Must be built, then uploaded under this asset name.
    Build(String),
}

impl Action {
    /// The asset name either way.
    #[must_use]
    pub(crate) fn asset(&self) -> &str {
        match self {
            Self::Fetch(name) | Self::Build(name) => name,
        }
    }
}

/// Decide, for each wanted artifact, whether it can be fetched or must be built.
///
/// `available` is the set of asset names already in the cache. Pure, so the
/// network stays in the workflow and this stays testable.
pub(crate) fn plan(wanted: &[Wanted], available: &BTreeSet<String>) -> Vec<(Wanted, Action)> {
    wanted
        .iter()
        .map(|item| {
            let name = item.recipe.artifact_name(&item.folder);
            let action = if available.contains(&name) {
                Action::Fetch(name)
            } else {
                Action::Build(name)
            };
            (item.clone(), action)
        })
        .collect()
}

/// Enumerate every app in an SDK checkout and the recipe each would be built by.
///
/// Reads `AppID`s straight from the sources, so no build is needed to know what the
/// cache should contain.
pub(crate) fn wanted_from_sdk(
    sdk: &Path,
    sdk_rev: &str,
    toolchain: &str,
    version: Version,
) -> Result<Vec<Wanted>> {
    let apps_dir = sdk.join("Examples").join("Apps");
    let mut names: Vec<String> = fs::read_dir(&apps_dir)
        .with_context(|| format!("reading {}", apps_dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();

    let mut wanted = Vec::new();
    for folder in names {
        let root = apps_dir.join(&folder);
        // An app directory without a CMake project is not buildable. Skip it with
        // a note rather than failing the whole plan: the SDK tree carries
        // work-in-progress apps.
        let project = match find_project(&root) {
            Ok(project) => project,
            Err(err) => {
                eprintln!("  skipping {folder}: {err}");
                continue;
            }
        };
        let declared = match read_declared(&project) {
            Ok(declared) => declared,
            Err(err) => {
                eprintln!("  skipping {folder}: {err}");
                continue;
            }
        };
        wanted.push(Wanted {
            app_id: declared.app_id,
            folder: folder.clone(),
            recipe: Recipe {
                app_source: format!("sdk:{sdk_rev}:Examples/Apps/{folder}"),
                sdk_rev: sdk_rev.to_owned(),
                toolchain: toolchain.to_owned(),
                build_version: version,
                flags: flags_id(),
            },
        });
    }
    Ok(wanted)
}

/// One line of the emitted plan.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlanItem {
    pub app_id: String,
    pub folder: String,
    pub version: String,
    pub recipe: String,
    pub asset: String,
    /// `"fetch"` or `"build"`, for a workflow to branch on.
    pub action: &'static str,
}

/// Render a plan as JSON for a workflow to consume.
#[must_use]
pub(crate) fn plan_items(planned: &[(Wanted, Action)]) -> Vec<PlanItem> {
    planned
        .iter()
        .map(|(wanted, action)| PlanItem {
            app_id: wanted.app_id.to_string(),
            folder: wanted.folder.clone(),
            version: wanted.recipe.build_version.to_string(),
            recipe: wanted.recipe.key(),
            asset: action.asset().to_owned(),
            action: match action {
                Action::Fetch(_) => "fetch",
                Action::Build(_) => "build",
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> Recipe {
        Recipe {
            app_source: "sdk:apps-v1.3.0:Examples/Apps/Alarm".into(),
            sdk_rev: "apps-v1.3.0".into(),
            toolchain: "sha256:7e07c508".into(),
            build_version: Version::new(1, 3, 0),
            flags: "-fmacro-prefix-map".into(),
        }
    }

    const ALARM: AppId = AppId::new(0xA19C_2A7E_4F8B_6D31);

    #[test]
    fn the_key_is_stable_for_the_same_recipe() {
        assert_eq!(recipe().key(), recipe().key());
        assert_eq!(recipe().key().len(), 16);
    }

    #[test]
    fn every_field_changes_the_key() {
        let base = recipe().key();
        let mut variants = Vec::new();

        let mut r = recipe();
        r.app_source = "git:https://example.test/x@abc:.".into();
        variants.push(r.key());

        let mut r = recipe();
        r.sdk_rev = "apps-v1.2.0".into();
        variants.push(r.key());

        // The toolchain matters: the same source through a different compiler is
        // not the same artifact.
        let mut r = recipe();
        r.toolchain = "sha256:deadbeef".into();
        variants.push(r.key());

        // BUILD_VERSION is compiled into the binary.
        let mut r = recipe();
        r.build_version = Version::new(1, 2, 0);
        variants.push(r.key());

        let mut r = recipe();
        r.flags = "-O0".into();
        variants.push(r.key());

        for (index, key) in variants.iter().enumerate() {
            assert_ne!(*key, base, "variant {index} did not change the key");
        }
        // And they are all distinct from each other.
        let unique: BTreeSet<_> = variants.iter().collect();
        assert_eq!(unique.len(), variants.len());
    }

    #[test]
    fn field_values_cannot_be_confused_by_concatenation() {
        // A canonical form that merely joined values could hash these the same.
        let mut a = recipe();
        a.sdk_rev = "one".into();
        a.toolchain = "two".into();
        let mut b = recipe();
        b.sdk_rev = "one\ntoolchain=two".into();
        b.toolchain = String::new();
        assert_ne!(a.key(), b.key());
    }

    #[test]
    fn the_artifact_name_is_readable_and_recipe_keyed() {
        let name = recipe().artifact_name("Alarm");
        assert!(name.starts_with("Alarm-1.3.0-"), "{name}");
        assert!(name.ends_with(&format!("{}.uapp", recipe().key())));
        assert_eq!(std::path::Path::new(&name).extension().unwrap(), "uapp");
        // No path separators: release assets are a flat namespace.
        assert!(!name.contains('/'));
    }

    #[test]
    fn a_label_cannot_introduce_a_path_or_spaces() {
        // A display name could be anything -- "AVG / R HR" really exists.
        let name = recipe().artifact_name("AVG / R HR");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(' '), "{name}");
        assert!(name.starts_with("AVG___R_HR-"), "{name}");
    }

    #[test]
    fn the_label_does_not_affect_uniqueness() {
        // Same recipe, different labels: the key is what distinguishes artifacts,
        // so a renamed folder does not silently alias a different build.
        let recipe = recipe();
        assert!(
            recipe
                .artifact_name("One")
                .ends_with(&format!("{}.uapp", recipe.key()))
        );
        assert!(
            recipe
                .artifact_name("Two")
                .ends_with(&format!("{}.uapp", recipe.key()))
        );
    }

    #[test]
    fn planning_fetches_what_is_cached_and_builds_what_is_not() {
        let wanted = vec![
            Wanted {
                app_id: ALARM,
                folder: "Alarm".into(),
                recipe: recipe(),
            },
            Wanted {
                app_id: AppId::new(0xA135_8F7C_2E9D_4BA6),
                folder: "GlanceHR".into(),
                recipe: recipe(),
            },
        ];
        let available = BTreeSet::from([wanted[0].recipe.artifact_name(&wanted[0].folder)]);

        let planned = plan(&wanted, &available);
        assert!(matches!(planned[0].1, Action::Fetch(_)));
        assert!(matches!(planned[1].1, Action::Build(_)));
        assert_eq!(
            planned[0].1.asset(),
            planned[0].0.recipe.artifact_name(&planned[0].0.folder)
        );
    }

    #[test]
    fn a_toolchain_bump_invalidates_the_cache_rather_than_reusing_it() {
        let old = Wanted {
            app_id: ALARM,
            folder: "Alarm".into(),
            recipe: recipe(),
        };
        let available = BTreeSet::from([old.recipe.artifact_name(&old.folder)]);

        let mut bumped = old.clone();
        bumped.recipe.toolchain = "sha256:0000newimage".into();

        assert!(matches!(plan(&[old], &available)[0].1, Action::Fetch(_)));
        assert!(matches!(plan(&[bumped], &available)[0].1, Action::Build(_)));
    }

    #[test]
    fn an_empty_cache_builds_everything() {
        let wanted = vec![Wanted {
            app_id: ALARM,
            folder: "Alarm".into(),
            recipe: recipe(),
        }];
        let planned = plan(&wanted, &BTreeSet::new());
        assert!(matches!(planned[0].1, Action::Build(_)));
    }
}
