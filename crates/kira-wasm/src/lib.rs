//! Browser bindings for [`kira_core`].
//!
//! The page keeps a [`Store`], which owns the catalogue and the user's version
//! pins, and asks it for views to render. Only what a browser genuinely must do
//! itself stays in JavaScript: the File System Access API, `IndexedDB` and the DOM.
//! Everything about the `.uapp` format, version selection, diffing and script
//! generation is the same Rust the catalogue build uses.

use std::collections::BTreeMap;

use kira_core::catalog::{self, Catalog, Release, Target};
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
    service_len: usize,
    gui_len: Option<usize>,
}

/// Read a `.uapp` header.
///
/// Pass just the first 48 bytes when scanning a watch: reading whole files off a
/// USB volume merely to list what is installed would be needlessly slow. Giving
/// `total_len` additionally derives the GUI image length and rejects a file too
/// small for what the header declares.
///
/// # Errors
/// If the slice is too short, or `total_len` contradicts the header.
#[wasm_bindgen]
pub fn read_header(bytes: &[u8], total_len: Option<usize>) -> Result<JsValue, JsError> {
    let header = Header::parse(bytes).map_err(js_err)?;
    let gui_len = match total_len {
        Some(total) => Some(header.gui_len(total).map_err(js_err)?),
        None => None,
    };
    to_js(&HeaderView {
        app_id: header.app_id,
        name: header.name.clone(),
        version: header.version,
        libc_version: header.libc_version,
        app_type: header.app_type(),
        autostart: header.autostart(),
        service_len: header.service_len,
        gui_len,
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
    /// Byte-derived summary, e.g. "code unchanged since 1.2.0".
    history: String,
    /// Version currently selected for this app.
    selected: Version,
    /// True when another catalogue entry shares this display name, so the UI can
    /// disambiguate by id instead of showing identical-looking cards.
    ambiguous_name: bool,
    /// Set when another app owns this on-device folder with newer versions, in
    /// which case this one cannot be installed alongside it.
    superseded_by: Option<AppId>,
    /// Who publishes this app, when it is not upstream's. Its presence is what
    /// makes an entry a submission; it is not a rank.
    publisher: Option<&'a catalog::Publisher>,
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
    pinned: BTreeMap<AppId, Version>,
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
            .map(|app| AppView {
                app_id: app.app_id,
                name: &app.name,
                app_type: app.app_type,
                folder: &app.folder,
                versions: &app.versions,
                icon: app.icon.as_deref(),
                icon_small: app.icon_small.as_deref(),
                history: app.describe_history(),
                selected: self
                    .pinned
                    .get(&app.app_id)
                    .copied()
                    .filter(|v| app.find(*v).is_some())
                    .unwrap_or_else(|| app.latest().version),
                ambiguous_name: self.ambiguous.contains(&app.name),
                superseded_by: app.superseded_by,
                publisher: app.publisher.as_ref(),
                retired: app.retired.as_deref(),
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
            Some(raw) => {
                let version: Version = raw.parse().map_err(js_err)?;
                if app.find(version).is_none() {
                    return Err(JsError::new(&format!(
                        "{} has no version {version}",
                        app.name
                    )));
                }
                // Following the newest is the absence of a pin, so a stale pin
                // cannot survive a release that makes it the latest.
                if version == app.latest().version {
                    self.pinned.remove(&id);
                } else {
                    self.pinned.insert(id, version);
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
    /// `kind` is `"powershell"` or `"shell"`.
    ///
    /// # Errors
    /// If `installed` is malformed, or `kind` is unknown.
    pub fn script(
        &self,
        kind: &str,
        installed: JsValue,
        base_url: &str,
    ) -> Result<String, JsError> {
        let installed: Vec<Installed> =
            serde_wasm_bindgen::from_value(installed).map_err(js_err)?;
        let plan = plan::build(&self.resolve(), &installed);
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

    fn resolve(&self) -> Vec<Target> {
        catalog::resolve_targets(&self.catalog, &self.pinned)
    }
}
