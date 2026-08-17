//! Browser bindings for [`kira_core`].
//!
//! The page keeps a [`Store`], which owns the catalogue and the user's version
//! pins, and asks it for views to render. Only what a browser genuinely must do
//! itself stays in JavaScript: the File System Access API, `IndexedDB` and the DOM.
//! Everything about the `.uapp` format, version selection, diffing and script
//! generation is the same Rust the catalogue build uses.

use std::collections::BTreeMap;

use kira_core::catalog::{self, Catalog, Release, Target};
use kira_core::config;
use kira_core::notes;
use kira_core::plan::{self, Installed, Plan, ScriptConfig};
use kira_core::uapp::{AppId, AppType, CRC_LEN, HEADER_LEN, Header, Uapp, Version};
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Convert any error into something JavaScript can throw.
fn js_err(error: impl std::fmt::Display) -> JsError {
    JsError::new(&error.to_string())
}

fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsError> {
    serde_wasm_bindgen::to_value(value).map_err(js_err)
}

/// What the header scan reports for one installed `.uapp`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeaderView {
    app_id: AppId,
    name: String,
    version: Version,
    libc_version: Version,
    #[serde(rename = "type")]
    app_type: AppType,
    autostart: bool,
    /// Whether this file is a code-less variant alias rather than an app.
    ///
    /// From the flag word, so a header-only scan can see it. What the alias
    /// *says* — its target and config — needs the whole file.
    variant_alias: bool,
    service_len: usize,
    trailing_len: Option<usize>,
}

/// Read a `.uapp` header.
///
/// Pass just the first 48 bytes when scanning a watch: reading whole files off a
/// USB volume merely to list what is installed would be needlessly slow. Giving
/// `total_len` additionally derives the length of the trailing region — the GUI
/// image, or an alias descriptor — and rejects a file too small for what the
/// header declares.
///
/// # Errors
/// If the slice is too short, or `total_len` contradicts the header.
#[wasm_bindgen]
pub fn read_header(bytes: &[u8], total_len: Option<usize>) -> Result<JsValue, JsError> {
    let header = Header::parse(bytes).map_err(js_err)?;
    let trailing_len = match total_len {
        Some(total) => Some(header.trailing_len(total).map_err(js_err)?),
        None => None,
    };
    to_js(&HeaderView {
        app_id: header.app_id,
        name: header.name.clone(),
        version: header.version,
        libc_version: header.libc_version,
        app_type: header.app_type(),
        autostart: header.autostart(),
        variant_alias: header.is_variant_alias(),
        service_len: header.service_len,
        trailing_len,
    })
}

/// Whether a complete `.uapp`'s CRC-32 footer matches its content.
///
/// A file failing this is dropped *silently* by the watch kernel — the app simply
/// never appears in the launcher — so always check before writing one to a device.
///
/// # Errors
/// If the bytes are not a parseable `.uapp`.
#[wasm_bindgen]
pub fn crc_is_valid(bytes: &[u8]) -> Result<bool, JsError> {
    Ok(Uapp::parse(bytes).map_err(js_err)?.verify_crc().is_valid())
}

#[derive(Serialize)]
struct Bounds {
    start: usize,
    end: usize,
}

/// Byte range of the code within a `.uapp`: everything between the header and the
/// CRC footer.
///
/// Returned as bounds rather than bytes so the caller can hash in place without a
/// copy, while the format's layout stays in one place.
///
/// # Errors
/// If the file is too short to hold a header and a footer.
#[wasm_bindgen]
pub fn payload_bounds(total_len: usize) -> Result<JsValue, JsError> {
    if total_len < HEADER_LEN + CRC_LEN {
        return Err(JsError::new("file is too short to be a .uapp"));
    }
    to_js(&Bounds {
        start: HEADER_LEN,
        end: total_len - CRC_LEN,
    })
}

/// Pick apart the `app_source` a submission's recipe records: the repository,
/// the exact commit, and the path within it.
///
/// Exported rather than done in the page so the recipe format has one reader.
/// Returns `undefined` for anything that is not a submission's source, notably an
/// SDK app's, which names no repository of its own.
///
/// # Errors
/// If the parsed value cannot be handed to JavaScript.
#[wasm_bindgen]
pub fn source_ref(app_source: &str) -> Result<JsValue, JsError> {
    match catalog::parse_source(app_source) {
        Some(parsed) => to_js(&parsed),
        None => Ok(JsValue::UNDEFINED),
    }
}

/// An app with its rendered history line, ready to display.
///
/// Fields are listed rather than flattened from [`App`]: `serde_wasm_bindgen`
/// silently drops `#[serde(flatten)]`, which is worth avoiding entirely.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppView<'a> {
    app_id: AppId,
    name: &'a str,
    #[serde(rename = "type")]
    app_type: AppType,
    folder: &'a str,
    versions: &'a [kira_core::catalog::VersionEntry],
    icon: Option<&'a str>,
    icon_small: Option<&'a str>,
    /// Byte-derived summary, e.g. "code unchanged since 1.2.0". For a variant
    /// alias it names what its target did too, since the alias's own bytes
    /// cannot say whether its behaviour moved.
    history: String,
    /// What the selected build being a variant alias means, when it is one.
    ///
    /// A whole sentence rather than the descriptor's fields, because naming the
    /// target and saying it has to be installed too is a rule, and rules do not
    /// live in the page. Render as text.
    variant: Option<String>,
    /// Version currently selected for this app.
    /// The chosen build's label, e.g. `1.4.0` or `1.4.0-rc1`.
    selected: String,
    /// The build offered by default, which is not always the highest-precedence
    /// one: see `App::latest`. The page marks this rather than re-deriving it.
    latest_label: String,
    /// True when another catalogue entry shares this display name, so the UI can
    /// disambiguate by id instead of showing identical-looking cards.
    ambiguous_name: bool,
    /// Set when another app owns this on-device folder with newer versions, in
    /// which case this one cannot be installed alongside it.
    superseded_by: Option<AppId>,
    /// Who publishes this app, when it is not upstream's. Its presence is what
    /// makes an entry a submission; it is not a rank.
    publisher: Option<&'a catalog::Publisher>,
    /// A settings file the app reads from its own folder, if it declares one.
    config: Option<&'a config::Spec>,
    /// Why the app is no longer offered, if it is not.
    retired: Option<&'a str>,
}

/// A release, with its prose sorted and its effect on the apps worked out.
///
/// Both are done here rather than in the page: which lines can reach a watch is a
/// judgement, and which apps a release actually changed comes from comparing
/// binaries. Neither belongs in DOM code.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseView<'a> {
    tag: &'a str,
    published_at: Option<&'a str>,
    url: Option<&'a str>,
    is_prerelease: bool,
    app_count: usize,
    /// The body verbatim, so the page can still offer the original.
    notes: Option<&'a str>,
    /// The body split by what it can affect.
    changes: notes::Notes,
    /// What the release did to the apps, from the binaries.
    effect: catalog::ReleaseEffect,
}

impl<'a> ReleaseView<'a> {
    fn of(release: &'a Release, apps: &[catalog::App]) -> Self {
        Self {
            tag: &release.tag,
            published_at: release.published_at.as_deref(),
            url: release.url.as_deref(),
            is_prerelease: release.is_prerelease,
            app_count: release.app_count,
            notes: release.notes.as_deref(),
            changes: release
                .notes
                .as_deref()
                .map(notes::parse)
                .unwrap_or_default(),
            effect: catalog::changed_in(apps, &release.tag),
        }
    }
}

/// A plan entry with its label rendered.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryView<'a> {
    app: &'a Target,
    status: plan::Status,
    installed: Option<&'a Installed>,
    identical_payload: bool,
    /// Which build is on the watch: Kira's, the vendor's, or neither.
    recognised: plan::Recognised,
    /// What verifying would find on the device for this app.
    ///
    /// Exposed rather than left for the page to work out from the hashes, which
    /// is how the vendor's own binaries came to be reported as mismatches.
    verdict: plan::Verdict,
    /// An app occupying this one's on-device folder that is not this app.
    blocking: Option<&'a Installed>,
    /// Why this entry is in the plan, e.g. "1.2.0 → 1.3.0".
    describe: String,
    /// Whether acting on this entry would write to the watch.
    ///
    /// Exposed rather than left for the page to infer from `status`, which had
    /// the UI silently dropping corrupt installs that the planner does offer.
    is_actionable: bool,
}

/// A whole plan, with the counts a UI would otherwise recompute.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanView<'a> {
    entries: Vec<EntryView<'a>>,
    foreign: &'a [Installed],
    actionable: usize,
    restamps: usize,
    install: usize,
    update: usize,
    current: usize,
    /// Installs the watch is ignoring because they fail their own CRC.
    ///
    /// Counted separately because they are in none of the three above, and a
    /// summary built from those alone under-reports the work.
    corrupt: usize,
}

impl<'a> PlanView<'a> {
    fn of(plan: &'a Plan) -> Self {
        use plan::Status;
        let count = |status: Status| plan.entries.iter().filter(|e| e.status == status).count();
        Self {
            entries: plan
                .entries
                .iter()
                .map(|entry| EntryView {
                    app: &entry.app,
                    status: entry.status,
                    installed: entry.installed.as_ref(),
                    identical_payload: entry.identical_payload,
                    recognised: entry.recognised,
                    verdict: entry.verdict(),
                    blocking: entry.blocking.as_ref(),
                    describe: entry.describe(),
                    is_actionable: entry.is_actionable(),
                })
                .collect(),
            foreign: &plan.foreign,
            actionable: plan.actionable().count(),
            restamps: plan.restamp_count(),
            install: count(Status::Install),
            update: count(Status::Update),
            current: count(Status::Current),
            corrupt: count(Status::Corrupt),
        }
    }
}

/// The catalogue, plus which version of each app is selected.
#[wasm_bindgen]
pub struct Store {
    catalog: Catalog,
    pinned: BTreeMap<AppId, String>,
    ambiguous: Vec<String>,
}

#[wasm_bindgen]
impl Store {
    /// Take a parsed `catalog.json`.
    ///
    /// The caller passes the result of `JSON.parse` rather than a string: the
    /// browser already has a JSON parser, and linking a second one in here cost
    /// about 16 kB gzipped.
    ///
    /// # Errors
    /// If the value does not match the schema this build expects.
    #[wasm_bindgen(constructor)]
    pub fn new(catalog: JsValue) -> Result<Store, JsError> {
        let catalog: Catalog = serde_wasm_bindgen::from_value(catalog).map_err(js_err)?;
        if catalog.schema != catalog::SCHEMA {
            return Err(JsError::new(&format!(
                "unsupported catalogue schema {} (expected {})",
                catalog.schema,
                catalog::SCHEMA
            )));
        }
        let ambiguous = catalog.ambiguous_names();
        Ok(Self {
            catalog,
            pinned: BTreeMap::new(),
            ambiguous,
        })
    }

    /// When the catalogue was built.
    #[wasm_bindgen(getter, js_name = generated)]
    #[must_use]
    pub fn generated(&self) -> String {
        self.catalog.generated.clone()
    }

    /// How many apps it holds.
    #[wasm_bindgen(getter, js_name = appCount)]
    #[must_use]
    pub fn app_count(&self) -> usize {
        self.catalog.apps.len()
    }

    /// How many versions across all apps.
    #[wasm_bindgen(getter, js_name = versionCount)]
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.catalog.apps.iter().map(|a| a.versions.len()).sum()
    }

    /// How many releases it covers.
    #[wasm_bindgen(getter, js_name = releaseCount)]
    #[must_use]
    pub fn release_count(&self) -> usize {
        self.catalog.releases.len()
    }

    /// Every app, with its history line and current selection.
    ///
    /// # Errors
    /// If the values cannot be handed to JavaScript.
    pub fn apps(&self) -> Result<JsValue, JsError> {
        let views: Vec<AppView<'_>> = self
            .catalog
            .apps
            .iter()
            .map(|app| {
                // The build the card is about, which is the pin where there is
                // one. `resolve_targets` chooses the same way; this is the same
                // question asked of one app rather than all of them.
                let chosen = self
                    .pinned
                    .get(&app.app_id)
                    .and_then(|label| app.find(label))
                    .unwrap_or_else(|| app.latest());
                AppView {
                    app_id: app.app_id,
                    name: &app.name,
                    app_type: app.app_type,
                    folder: &app.folder,
                    versions: &app.versions,
                    icon: app.icon.as_deref(),
                    icon_small: app.icon_small.as_deref(),
                    history: self.catalog.describe_history(app),
                    variant: self.catalog.describe_variant(chosen),
                    selected: chosen.label(),
                    latest_label: app.latest().label(),
                    ambiguous_name: self.ambiguous.contains(&app.name),
                    superseded_by: app.superseded_by,
                    publisher: app.publisher.as_ref(),
                    config: app.config.as_ref(),
                    retired: app.retired.as_deref(),
                }
            })
            .collect();
        to_js(&views)
    }

    /// Release metadata, newest first.
    ///
    /// Notes are upstream Markdown: render them as text, never as HTML.
    ///
    /// # Errors
    /// If the values cannot be handed to JavaScript.
    pub fn releases(&self) -> Result<JsValue, JsError> {
        let views: Vec<ReleaseView<'_>> = self
            .catalog
            .releases
            .iter()
            .map(|release| ReleaseView::of(release, &self.catalog.apps))
            .collect();
        to_js(&views)
    }

    /// Select a version for an app, or pass nothing to follow the newest.
    ///
    /// # Errors
    /// If the app or version is unknown, rather than silently doing nothing.
    pub fn pin(&mut self, app_id: &str, version: Option<String>) -> Result<(), JsError> {
        let id: AppId = app_id.parse().map_err(js_err)?;
        let app = self
            .catalog
            .apps
            .iter()
            .find(|a| a.app_id == id)
            .ok_or_else(|| JsError::new(&format!("no app with id {app_id}")))?;

        match version {
            None => {
                self.pinned.remove(&id);
            }
            Some(label) => {
                // A label, not a version: `1.4.0-rc1` and `1.4.0` are different
                // builds stamping the same version number.
                if app.find(&label).is_none() {
                    return Err(JsError::new(&format!(
                        "{} has no version {label}",
                        app.name
                    )));
                }
                // Following the newest is the absence of a pin, so a stale pin
                // cannot survive a release that makes it the latest.
                if label == app.latest().label() {
                    self.pinned.remove(&id);
                } else {
                    self.pinned.insert(id, label);
                }
            }
        }
        Ok(())
    }

    /// The selected version of every app, flattened.
    ///
    /// # Errors
    /// If the values cannot be handed to JavaScript.
    pub fn targets(&self) -> Result<JsValue, JsError> {
        to_js(&self.resolve())
    }

    /// Diff the selection against what is installed on a watch.
    ///
    /// `installed` is an array of `{appId, folder, file, name, version, size,
    /// extraUapps, payloadSha256}`.
    ///
    /// # Errors
    /// If `installed` is not of that shape.
    pub fn plan(&self, installed: JsValue) -> Result<JsValue, JsError> {
        let installed: Vec<Installed> =
            serde_wasm_bindgen::from_value(installed).map_err(js_err)?;
        let plan = plan::build(&self.resolve(), &installed);
        to_js(&PlanView::of(&plan))
    }

    /// Generate a standalone installer, for browsers that cannot write to the
    /// drive themselves.
    ///
    /// `kind` is `"powershell"` or `"shell"`. `chosen` is an array of `AppId`
    /// strings to write, or `undefined` for everything the plan offers — a
    /// script that ignored the choice made on the page would do more than the
    /// page said it would.
    ///
    /// # Errors
    /// If `installed` is malformed, `chosen` holds something that is not an
    /// `AppId`, or `kind` is unknown.
    pub fn script(
        &self,
        kind: &str,
        installed: JsValue,
        base_url: &str,
        chosen: JsValue,
    ) -> Result<String, JsError> {
        let installed: Vec<Installed> =
            serde_wasm_bindgen::from_value(installed).map_err(js_err)?;
        let mut plan = plan::build(&self.resolve(), &installed);
        if !chosen.is_undefined() && !chosen.is_null() {
            let chosen: Vec<String> = serde_wasm_bindgen::from_value(chosen).map_err(js_err)?;
            let ids = chosen
                .iter()
                .map(|id| id.parse::<AppId>())
                .collect::<Result<std::collections::BTreeSet<_>, _>>()
                .map_err(js_err)?;
            plan = plan.only(&ids);
        }
        let config = ScriptConfig {
            base_url: base_url.to_owned(),
            ..ScriptConfig::default()
        };
        match kind {
            "powershell" => Ok(plan::powershell(&plan, &config)),
            "shell" => Ok(plan::shell(&plan, &config)),
            other => Err(JsError::new(&format!(
                "unknown script kind {other}: expected \"powershell\" or \"shell\""
            ))),
        }
    }

    /// Why one value is unusable, or nothing when it is fine.
    ///
    /// Separate from [`Self::config_document`] so the form can say what is wrong
    /// beside the field it is wrong about, while it is being typed, rather than
    /// only at the point of writing to a watch.
    ///
    /// # Errors
    /// If the app is unknown, declares no config, or has no such field — all of
    /// which are page bugs rather than anything the user did.
    #[wasm_bindgen(js_name = configCheck)]
    pub fn config_check(
        &self,
        app_id: &str,
        path: &str,
        value: &str,
    ) -> Result<Option<String>, JsError> {
        let spec = self.config_spec(app_id)?;
        let field = spec
            .fields
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| JsError::new(&format!("{app_id} declares no field {path}")))?;
        Ok(config::check_value(field, value).err())
    }

    /// The finished file, ready to write into `Apps/<Folder>/`.
    ///
    /// Assembled here rather than in the page for the same reason the installers
    /// are: it is the part where a mistake reaches a device, and here it is
    /// covered by tests that run without a browser.
    ///
    /// # Errors
    /// If the app declares no config, a value is missing or rejected, or the
    /// result would be too large. The message is meant to be shown as-is.
    #[wasm_bindgen(js_name = configDocument)]
    pub fn config_document(&self, app_id: &str, values: JsValue) -> Result<String, JsError> {
        let spec = self.config_spec(app_id)?;
        let values: BTreeMap<String, String> =
            serde_wasm_bindgen::from_value(values).map_err(js_err)?;
        config::document(spec, &values).map_err(|problem| JsError::new(&problem))
    }

    fn config_spec(&self, app_id: &str) -> Result<&config::Spec, JsError> {
        let id: AppId = app_id.parse().map_err(js_err)?;
        self.catalog
            .apps
            .iter()
            .find(|a| a.app_id == id)
            .and_then(|a| a.config.as_ref())
            .ok_or_else(|| JsError::new(&format!("{app_id} declares no settings file")))
    }

    fn resolve(&self) -> Vec<Target> {
        catalog::resolve_targets(&self.catalog, &self.pinned)
    }
}
