//! `app-manifest.json`: the package metadata an app carries beside its source.
//!
//! The SDK landed app configuration, and the declaration lives here — in the
//! app's own tree, at the commit Kira builds. That is the whole reason this
//! module exists. Everything else on a card is derived from a binary Kira built
//! itself; a settings declaration used to be the exception, asserted in a
//! submission's registry manifest with nothing to check it against. Now it comes
//! out of the pinned source, which makes it part of the recipe: the same commit
//! that produced the bytes produced the declaration, and anybody can fetch both.
//!
//! Only the configuration keys are read. A manifest carries plenty that belongs
//! to UNA's own store — the icon, the FIT capability flags, the custom measures —
//! and none of it has a use here.
//!
//! The file itself is never copied to a watch, which is the SDK's rule and not
//! Kira's habit: it is what the *phone* reads to know what to ask for. What
//! reaches the watch is the values file the answers go into, which
//! [`kira_core::config`] assembles.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use kira_core::config::{self, Spec};
use kira_core::uapp::Version;
use serde::{Deserialize, Serialize};

/// Name of the file, in the app root beside `Software/`.
pub(crate) const MANIFEST_NAME: &str = "app-manifest.json";

/// Attributes every field may carry, whatever its type.
const COMMON_KEYS: &[&str] = &[
    "id",
    "type",
    "label",
    "description",
    "default",
    "required",
    "validationMessage",
];

/// The extra attributes each type allows, beyond [`COMMON_KEYS`].
fn extra_keys(field_type: &str) -> &'static [&'static str] {
    match field_type {
        "string" => &["minLength", "maxLength", "pattern"],
        "int" | "float" => &["min", "max", "unit"],
        _ => &[],
    }
}

/// What Kira takes from one app's manifest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Declaration {
    /// The settings file and fields, when the app declares any.
    pub config: Option<Spec>,
    /// The version the manifest claims, when it states one.
    ///
    /// Checked against the version being built: a manifest that names a
    /// different one belongs to a different build of the app, and the
    /// declaration Kira would publish beside these bytes would be labelled with
    /// a version its own source does not claim.
    pub app_version: Option<Version>,
}

/// How a build's declaration is carried through the artifact store.
///
/// An envelope rather than the manifest verbatim, because "this source declares
/// no configuration" has to be sayable. Without it, a missing sidecar would mean
/// either that or "built before Kira stored one", and the catalogue build cannot
/// tell those apart without fetching the source it deliberately does not have.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Sidecar {
    /// Envelope version, so this can change without being guessed at.
    pub kira: u32,
    /// The app's `app-manifest.json`, verbatim, or `null` when it has none.
    ///
    /// Verbatim so the catalogue build can run the same checks over it again,
    /// the way it re-verifies a stored `.uapp`'s CRC and identity rather than
    /// trusting that the build that made it was sound.
    pub app_manifest: Option<serde_json::Value>,
}

/// Envelope version this understands.
pub(crate) const SIDECAR_VERSION: u32 = 1;

impl Sidecar {
    /// Wrap what an app root declares, whether or not it declares anything.
    pub(crate) fn of(manifest: Option<serde_json::Value>) -> Self {
        Self {
            kira: SIDECAR_VERSION,
            app_manifest: manifest,
        }
    }

    /// Read a stored sidecar and check what it carries all over again.
    ///
    /// # Errors
    /// If the envelope is unreadable, of an unknown version, or the manifest
    /// inside it does not pass the same checks the build ran.
    pub(crate) fn read(path: &Path) -> Result<Declaration> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let sidecar: Self =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        ensure!(
            sidecar.kira == SIDECAR_VERSION,
            "{}: stored declaration is version {}, and this build understands \
             {SIDECAR_VERSION}",
            path.display(),
            sidecar.kira
        );
        match sidecar.app_manifest {
            None => Ok(Declaration {
                config: None,
                app_version: None,
            }),
            Some(value) => parse(&value).with_context(|| format!("in {}", path.display())),
        }
    }
}

/// Where an app root's manifest would be.
pub(crate) fn path_in(app_root: &Path) -> PathBuf {
    app_root.join(MANIFEST_NAME)
}

/// Read an app root's manifest, if it has one.
///
/// A missing file is not an error: an app that wants no configuration declares
/// none, and most of the catalogue predates the feature entirely.
///
/// # Errors
/// If the file exists but cannot be read, is not JSON, declares a
/// `manifest_version` this does not understand, or carries a configuration
/// declaration that would not work.
pub(crate) fn read(app_root: &Path) -> Result<(Declaration, Option<serde_json::Value>)> {
    let path = path_in(app_root);
    if !path.is_file() {
        return Ok((
            Declaration {
                config: None,
                app_version: None,
            },
            None,
        ));
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let declaration = parse(&value).with_context(|| format!("in {}", path.display()))?;
    Ok((declaration, Some(value)))
}

/// Check a manifest and take the two things Kira uses from it.
///
/// # Errors
/// If the manifest is not an object, its `manifest_version` is missing or
/// unknown, its configuration keys contradict each other, a field carries an
/// attribute its type does not have, or the declaration fails
/// [`config::check_spec`].
pub(crate) fn parse(value: &serde_json::Value) -> Result<Declaration> {
    let object = value
        .as_object()
        .context("app-manifest.json is not a JSON object")?;

    // First, and refused rather than guessed at: a newer manifest format may
    // have moved the very keys read below, and a reader that carried on would be
    // reporting a declaration nobody wrote. The same rule the values file's
    // `schema` follows.
    let declared = object
        .get("manifest_version")
        .context("app-manifest.json declares no manifest_version")?;
    let declared = declared
        .as_u64()
        .with_context(|| format!("manifest_version must be a whole number, not {declared}"))?;
    ensure!(
        u32::try_from(declared) == Ok(config::MANIFEST_VERSION),
        "manifest_version is {declared}; this understands {}. A newer manifest is not \
         guessed at",
        config::MANIFEST_VERSION
    );

    let app_version = match object.get("appVersion") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let text = value
                .as_str()
                .with_context(|| format!("appVersion must be a string, not {value}"))?;
            Some(
                text.parse::<Version>()
                    .map_err(|e| anyhow::anyhow!("appVersion {text:?}: {e}"))?,
            )
        }
    };

    Ok(Declaration {
        config: parse_config(object)?,
        app_version,
    })
}

/// The `configFile` and `configFields` pair, as a checked [`Spec`].
fn parse_config(object: &serde_json::Map<String, serde_json::Value>) -> Result<Option<Spec>> {
    let file = object.get("configFile");
    let fields = object.get("configFields");

    // §2: both keys are optional, and an app that wants no configuration
    // declares neither. One without the other is a broken package rather than
    // something to work around -- a file with no fields would never be written,
    // and fields with no file have nowhere to go.
    let (file, fields) = match (file, fields) {
        (None, None) => return Ok(None),
        (Some(file), None) => bail!("configFile is set to {file} but no configFields are declared"),
        (None, Some(_)) => bail!(
            "configFields are declared but configFile is missing: Kira needs a file name \
             to write them to"
        ),
        (Some(file), Some(fields)) => (file, fields),
    };

    let listed = fields.as_array().context("configFields must be an array")?;
    if listed.is_empty() {
        bail!("configFile is set but configFields is empty");
    }

    // Refused before the shape is trusted to a deserialiser, because
    // `additionalProperties: false` is a rule of the SDK's schema and serde's
    // internally tagged enums cannot carry `deny_unknown_fields`. A misspelled
    // `maxLenght` would otherwise be silently ignored, and the field would be
    // published with a bound nobody wrote.
    for (at, field) in listed.iter().enumerate() {
        let entry = field
            .as_object()
            .with_context(|| format!("configFields[{at}] is not an object"))?;
        let field_type = entry
            .get("type")
            .and_then(serde_json::Value::as_str)
            .with_context(|| format!("configFields[{at}] declares no type"))?;
        let allowed = extra_keys(field_type);
        for key in entry.keys() {
            ensure!(
                COMMON_KEYS.contains(&key.as_str()) || allowed.contains(&key.as_str()),
                "configFields[{at}]: {key:?} is not an attribute of a {field_type:?} field"
            );
        }
    }

    let spec = Spec {
        file: file
            .as_str()
            .with_context(|| format!("configFile must be a string, not {file}"))?
            .to_owned(),
        fields: serde_json::from_value(fields.clone()).context("reading configFields")?,
    };
    config::check_spec(&spec).map_err(|problem| anyhow::anyhow!("{problem}"))?;
    Ok(Some(spec))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Squash/app-manifest.json` from watch-apps, trimmed to the keys Kira
    /// reads plus enough of the rest to be a real manifest.
    const SQUASH: &str = r#"{
      "manifest_version": 1,
      "type": ["activity"],
      "name": "Squash",
      "icon": "icon.png",
      "binary": "Squash_0.6.0.uapp",
      "appVersion": "0.6.0",
      "minKernelVersion": "1.4.0",
      "requiredHardware": [],
      "description": "A squash session.",
      "id": "9E526672EAFB61B6",
      "supportsLaps": true,
      "customMeasures": [],
      "configFile": "input.json",
      "configFields": [
        {
          "id": "recordImu",
          "type": "bool",
          "label": "Record raw IMU",
          "description": "Stream the wrist sensor's raw 100 Hz motion to a CSV file.",
          "default": false
        }
      ]
    }"#;

    fn manifest(text: &str) -> Result<Declaration> {
        parse(&serde_json::from_str(text).expect("fixture is JSON"))
    }

    #[test]
    fn a_real_manifest_yields_the_declaration_and_the_version() {
        let read = manifest(SQUASH).expect("valid");
        assert_eq!(read.app_version, Some(Version::new(0, 6, 0)));
        let config = read.config.expect("declares config");
        assert_eq!(config.file, "input.json");
        assert_eq!(config.fields.len(), 1);
        assert_eq!(config.fields[0].id, "recordImu");
        assert_eq!(config.fields[0].kind.type_name(), "bool");
        assert_eq!(config.fields[0].default_text(), "false");
    }

    #[test]
    fn every_type_a_field_can_be_survives_the_trip() {
        // Verbatim shapes from Spin and the SDK's Waypoint tutorial: an int with
        // a unit, a float pair, a bool, and a string with a pattern.
        let text = r#"{
          "manifest_version": 1,
          "configFile": "app_config.json",
          "configFields": [
            {"id": "waypointName", "type": "string", "label": "Waypoint name",
             "description": "Shown on the watch while you navigate.",
             "default": "Waypoint", "minLength": 1, "maxLength": 16,
             "pattern": "[A-Za-z0-9 ]+",
             "validationMessage": "Up to 16 letters, digits and spaces."},
            {"id": "targetLatitude", "type": "float", "label": "Target latitude",
             "description": "Latitude in decimal degrees.", "default": 51.5072,
             "min": -90.0, "max": 90.0, "unit": "deg", "required": true},
            {"id": "autoLapMinutes", "type": "int", "label": "Auto lap",
             "description": "Split the ride into laps this many minutes apart.",
             "default": 0, "min": 0, "max": 60, "unit": "min"},
            {"id": "keepScreenLit", "type": "bool", "label": "Keep the screen lit",
             "description": "Hold the backlight on for the whole ride.",
             "default": false}
          ]
        }"#;
        let config = manifest(text).expect("valid").config.expect("declared");
        let types: Vec<&str> = config.fields.iter().map(|f| f.kind.type_name()).collect();
        assert_eq!(types, ["string", "float", "int", "bool"]);
        assert_eq!(config.fields[1].unit(), Some("deg"));
        assert!(config.fields[1].required);
    }

    #[test]
    fn an_app_that_declares_no_configuration_is_not_an_error() {
        let text = r#"{"manifest_version": 1, "name": "RunMap", "appVersion": "0.2.0"}"#;
        let read = manifest(text).expect("valid");
        assert!(read.config.is_none());
        assert_eq!(read.app_version, Some(Version::new(0, 2, 0)));
    }

    #[test]
    fn a_manifest_version_this_does_not_know_is_refused_rather_than_read() {
        for bad in [
            r#"{"manifest_version": 2, "configFile": "a.json"}"#,
            r#"{"manifest_version": "1"}"#,
            r#"{"name": "NoVersion"}"#,
        ] {
            assert!(manifest(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn one_configuration_key_without_the_other_is_refused() {
        for bad in [
            r#"{"manifest_version": 1, "configFile": "input.json"}"#,
            r#"{"manifest_version": 1, "configFile": "input.json", "configFields": []}"#,
            r#"{"manifest_version": 1, "configFields": [
                 {"id":"a","type":"bool","label":"A","description":"d","default":true}]}"#,
        ] {
            assert!(manifest(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn a_misspelled_attribute_is_refused_instead_of_ignored() {
        // The SDK's schema says additionalProperties: false, and this is why:
        // ignoring 'maxLenght' would publish a field with a bound nobody wrote.
        let text = r#"{
          "manifest_version": 1,
          "configFile": "input.json",
          "configFields": [
            {"id": "id1", "type": "string", "label": "ID", "description": "An id.",
             "default": "", "maxLength": 16, "maxLenght": 8}
          ]
        }"#;
        let err = manifest(text).expect_err("misspelled");
        assert!(format!("{err:#}").contains("maxLenght"), "{err:#}");
    }

    #[test]
    fn an_attribute_belonging_to_another_type_is_refused() {
        let text = r#"{
          "manifest_version": 1,
          "configFile": "input.json",
          "configFields": [
            {"id": "lit", "type": "bool", "label": "Lit", "description": "A switch.",
             "default": true, "unit": "min"}
          ]
        }"#;
        assert!(manifest(text).is_err());
    }

    #[test]
    fn a_declaration_that_would_not_work_is_refused_here_and_not_on_a_watch() {
        // check_spec's rules, reached through a manifest: a file name that is a
        // device on Windows, and a default outside the field's own bounds.
        let device = r#"{
          "manifest_version": 1,
          "configFile": "nul.json",
          "configFields": [
            {"id": "a", "type": "bool", "label": "A", "description": "d", "default": true}
          ]
        }"#;
        assert!(manifest(device).is_err());

        let bad_default = r#"{
          "manifest_version": 1,
          "configFile": "input.json",
          "configFields": [
            {"id": "lap", "type": "int", "label": "Lap", "description": "d",
             "default": 99, "min": 0, "max": 60}
          ]
        }"#;
        assert!(manifest(bad_default).is_err());
    }

    #[test]
    fn a_stored_sidecar_reads_back_as_the_declaration_that_made_it() {
        // The store contract, end to end: what `build-app` writes beside a
        // binary is what the catalogue build reads out of it.
        let dir =
            std::env::temp_dir().join(format!("kira-sidecar-{}-{}", std::process::id(), line!()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("Squash-0.6.0-abc123.manifest.json");

        let value: serde_json::Value = serde_json::from_str(SQUASH).expect("fixture is JSON");
        let text = serde_json::to_string_pretty(&Sidecar::of(Some(value))).expect("serialises");
        fs::write(&path, text).expect("writes");
        let read = Sidecar::read(&path).expect("reads");
        assert_eq!(read.config.expect("declared").file, "input.json");
        assert_eq!(read.app_version, Some(Version::new(0, 6, 0)));

        // An app that declares nothing says so, rather than looking like a
        // build from before Kira stored declarations at all.
        let none = serde_json::to_string(&Sidecar::of(None)).expect("serialises");
        fs::write(&path, none).expect("writes");
        assert!(Sidecar::read(&path).expect("reads").config.is_none());

        // An envelope from a future Kira is refused, not read past.
        fs::write(&path, r#"{"kira": 2, "appManifest": null}"#).expect("writes");
        assert!(Sidecar::read(&path).is_err());

        // And so is a manifest that would not pass the build's own checks: the
        // store is a release asset, not something this process computed.
        fs::write(
            &path,
            r#"{"kira": 1, "appManifest": {"manifest_version": 1,
                 "configFile": "nul.json",
                 "configFields": [{"id": "a", "type": "bool", "label": "A",
                                   "description": "d", "default": true}]}}"#,
        )
        .expect("writes");
        assert!(Sidecar::read(&path).is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_sidecar_round_trips_both_answers() {
        let value: serde_json::Value = serde_json::from_str(SQUASH).expect("fixture is JSON");
        let text = serde_json::to_string(&Sidecar::of(Some(value))).expect("serialises");
        let back: Sidecar = serde_json::from_str(&text).expect("parses");
        assert_eq!(back.kira, SIDECAR_VERSION);
        let read = parse(&back.app_manifest.expect("carried")).expect("valid");
        assert_eq!(read.app_version, Some(Version::new(0, 6, 0)));

        let none = serde_json::to_string(&Sidecar::of(None)).expect("serialises");
        let back: Sidecar = serde_json::from_str(&none).expect("parses");
        assert!(back.app_manifest.is_none());
    }
}
