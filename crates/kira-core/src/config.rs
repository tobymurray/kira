//! The configuration an app declares, and the values file that answers it.
//!
//! A watch with four buttons and no keyboard cannot be told a parkrun athlete
//! id, a latitude, or how many minutes to lap at. The SDK's answer is a
//! *declaration*: an app names a file in `app-manifest.json` and lists the
//! fields it holds — id, type, label, description, default, bounds — a companion
//! app collects the answers, writes them next to the `.uapp`, and
//! `SDK::AppConfig` reads them back at launch. `Docs/app-config-fields.md` in
//! the SDK is the specification, and section 9 of it is normative for the
//! companion app. This module is Kira's half of that contract.
//!
//! Kira is a companion app that goes over the cable rather than over BLE, and
//! that is the only difference: `Apps/<Folder>/` is writable from any desktop
//! and is where the app's own relative paths resolve, so the file lands in
//! exactly the place §4.1 names.
//!
//! **Kira invents no part of the format.** The file name, every id, every bound
//! and every default come from the app's own `app-manifest.json` at the commit
//! Kira built — so unlike the arrangement this replaced, a declaration is no
//! longer an assertion in a submission's registry manifest that nothing could
//! check. It is part of the recipe: it comes out of the pinned source, the SDK's
//! own `validate_app_config.py` checks it in the app's CI, and
//! [`check_spec`] checks it again here before anything acts on it.
//!
//! What is *not* derived is the answers. Those are typed into the page, and this
//! module is the single place that decides whether one may reach a device: the
//! checks below run in the order §3.1 fixes, and [`document`] refuses rather
//! than repairs. It has to be strict on its own account, because the app at the
//! other end is not: `SDK::AppConfig` clamps a number and truncates a string,
//! and it has no regular-expression engine at all, so a value that fails a
//! declared `pattern` is a value the app reads and acts on.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

mod pattern;

/// Value written as the values file's top-level `schema`.
///
/// The envelope's version, not the app's field set, and not the manifest's own
/// `manifest_version`. A reader that meets any other value must ignore the whole
/// file and use its defaults (§4.2), so writing anything else would be writing a
/// file every app is required to throw away.
pub const VALUES_SCHEMA: u32 = 1;

/// The only `manifest_version` this understands.
///
/// A manifest declaring anything else is refused rather than parsed on the
/// chance that the parts Kira reads did not change — the same rule §2 puts on
/// every reader of one.
pub const MANIFEST_VERSION: u32 = 1;

/// Largest values file that may be written, in bytes.
///
/// `SDK::AppConfig` holds the whole file while it parses, and refuses one over
/// this (§8), so a longer document is one the app will not read.
pub const MAX_DOCUMENT_BYTES: usize = 8192;

/// Most fields an app may declare (§8): the app's presence mask is 32 bits wide.
pub const MAX_FIELDS: usize = 32;

/// Ceiling on a string field's `maxLength`, in UTF-8 bytes (§8).
pub const MAX_STRING_BYTES: usize = 128;

/// Longest text a conforming writer can emit for one `float` (§8).
///
/// Sign, 39 integer digits, a point and nine significant digits: the bound the
/// SDK's own size accounting uses.
const MAX_FLOAT_TEXT: usize = 1 + 39 + 1 + 9;

/// Longest text an `int` can take: `-2147483648`.
///
/// Counted separately from a float's for one reason -- the SDK's tooling counts
/// it separately, and a declaration its checker passes must not be refused here.
const MAX_INT_TEXT: usize = "-2147483648".len();

/// Longest `label`, in characters (§8).
const MAX_LABEL_CHARS: usize = 32;

/// Longest `description`, in characters (§8).
const MAX_DESCRIPTION_CHARS: usize = 200;

/// Longest `validationMessage`, in characters (§8).
const MAX_MESSAGE_CHARS: usize = 120;

/// Longest `unit`, in characters (§8).
const MAX_UNIT_CHARS: usize = 8;

/// Longest `id`, in characters (§8).
const MAX_ID_CHARS: usize = 32;

/// The package metadata file, which never reaches the watch and so can never be
/// the values file either (§2.1).
const RESERVED_FILE_NAME: &str = "app-manifest.json";

/// What an app declares it reads, and from where.
///
/// The two keys of §2 — `configFile` and `configFields` — under the names the
/// catalogue publishes them by. Both are always present here: a declaration with
/// no fields is not published at all, because there would be nothing to fill in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    /// The values file's name, within `Apps/<Folder>/`. A bare name, never a
    /// path.
    pub file: String,
    /// Every field, in the order the app declared them, which is display order.
    pub fields: Vec<Field>,
}

/// One value the app asks for.
///
/// Serialised in the shape `app-manifest.json` uses, so what the catalogue
/// carries is the app's own declaration rather than a translation of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    /// Key in the values file, and the string the app passes to
    /// `SDK::AppConfig`. Stable for the life of the app: changing one discards
    /// whatever the owner had set (§7.3).
    pub id: String,
    /// Short name shown beside the input. Units belong in `unit`.
    pub label: String,
    /// A longer explanation, for someone who has never used the app.
    pub description: String,
    /// Whether the app needs a value before it does anything useful.
    ///
    /// Unlike the arrangement this replaced, `required` is now acted on rather
    /// than only presented: a required field that is empty stops the file being
    /// written, which is §9.2's rule for the install flow. It still has no
    /// bearing on downloading or installing the binary — somebody may want the
    /// `.uapp` now and the value later.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// What to say when a value fails any of this field's constraints.
    ///
    /// One message per field, phrased as the rule. Absent means Kira explains
    /// the particular constraint that failed instead (§3.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_message: Option<String>,
    /// The field's type, with the constraints that only that type can carry.
    #[serde(flatten)]
    pub kind: Kind,
}

/// A field's type and the bounds that belong to it (§2.4).
///
/// Internally tagged by `type`, which is how `app-manifest.json` spells it, so
/// the JSON round-trips through here unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum Kind {
    /// Text, bounded in UTF-8 bytes and optionally by a pattern.
    String {
        /// Value the app falls back to, and what the input is pre-filled with.
        default: String,
        /// Shortest usable value, in UTF-8 bytes. A value below it is unusable
        /// to the app, which falls back to the default and reports the field
        /// unset.
        #[serde(default, skip_serializing_if = "is_zero")]
        min_length: usize,
        /// Longest value, in UTF-8 bytes rather than characters: it is what the
        /// app sizes its buffer from.
        max_length: usize,
        /// A regular expression the whole value must match, in the restricted
        /// dialect of §3.2. Applied by Kira alone — the app has no engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
    },
    /// A switch.
    Bool {
        /// Value the app falls back to.
        default: bool,
    },
    /// A whole number, bounded both ways.
    Int {
        /// Value the app falls back to.
        default: i32,
        /// Inclusive lower bound. The app clamps to it.
        min: i32,
        /// Inclusive upper bound.
        max: i32,
        /// Presentation-only suffix, e.g. `bpm`. Never part of the value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    /// A number, bounded both ways, held as `binary32` on the watch.
    Float {
        /// Value the app falls back to.
        default: f32,
        /// Inclusive lower bound. The app clamps to it.
        min: f32,
        /// Inclusive upper bound.
        max: f32,
        /// Presentation-only suffix, e.g. `deg`. Never part of the value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
}

/// One answer, once it has been checked against its field.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A `string` field's value.
    Text(String),
    /// A `bool` field's value.
    Flag(bool),
    /// An `int` field's value.
    Whole(i32),
    /// A `float` field's value.
    Number(f32),
}

/// Why a value or a declaration cannot be used.
pub type Problem = String;

/// Whether a `minLength` is the default of zero, so it stays out of the
/// catalogue.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Kind {
    /// The type's name, as `app-manifest.json` spells it.
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::String { .. } => "string",
            Self::Bool { .. } => "bool",
            Self::Int { .. } => "int",
            Self::Float { .. } => "float",
        }
    }

    /// The suffix to render after the input, when the type can carry one.
    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        match self {
            Self::Int { unit, .. } | Self::Float { unit, .. } => unit.as_deref(),
            Self::String { .. } | Self::Bool { .. } => None,
        }
    }

    /// This field's default, as the page would show it in an input.
    #[must_use]
    pub fn default_text(&self) -> String {
        match self {
            Self::String { default, .. } => default.clone(),
            Self::Bool { default } => default.to_string(),
            Self::Int { default, .. } => default.to_string(),
            Self::Float { default, .. } => number_text(*default),
        }
    }

    /// This field's default, as a checked value.
    fn default_value(&self) -> Value {
        match self {
            Self::String { default, .. } => Value::Text(default.clone()),
            Self::Bool { default } => Value::Flag(*default),
            Self::Int { default, .. } => Value::Whole(*default),
            Self::Float { default, .. } => Value::Number(*default),
        }
    }
}

impl Field {
    /// The suffix to render after this field's input, if it has one.
    #[must_use]
    pub fn unit(&self) -> Option<&str> {
        self.kind.unit()
    }

    /// This field's default, as the page would show it in an input.
    #[must_use]
    pub fn default_text(&self) -> String {
        self.kind.default_text()
    }

    /// The message to show for a failure on this field.
    ///
    /// The declared one when there is one, because a good message states the
    /// whole rule and beats a per-constraint report; otherwise what Kira worked
    /// out, so a field is never reported with a bare "invalid" (§3.3).
    fn say(&self, worked_out: &str) -> Problem {
        match &self.validation_message {
            Some(declared) => format!("{}: {declared}", self.label),
            None => format!("{}: {worked_out}", self.label),
        }
    }
}

/// Plain decimal text for a number, as §9.6 requires a writer to emit.
///
/// No exponent, no thousands separator, `.` as the separator. Rust's own float
/// formatting never reaches for exponent notation, which is the property this
/// relies on; the shortest round-tripping form is what it gives, so a value
/// typed as `51.5072` is written back as `51.5072` rather than as the nine
/// significant digits `SDK::AppConfig` would use.
#[must_use]
pub fn number_text(value: f32) -> String {
    let text = format!("{value}");
    // Rust prints an integral float without a fractional part, which is exactly
    // what a float field should carry: §4.3 accepts an integer literal for one.
    text
}

/// Check one answer against its field, in the order §3.1 fixes.
///
/// `text` is what the page holds for the field: empty means the user has not
/// supplied one, which is [`None`] rather than an error unless the field is
/// required. A `bool` is spelled `true` or `false`.
///
/// # Errors
/// Returns the reason, phrased for whoever has to retype the value: the field's
/// own `validationMessage` when it declares one.
pub fn check_value(field: &Field, text: &str) -> Result<Option<Value>, Problem> {
    if text.is_empty() {
        if !field.required {
            // §9.3: emptying an optional field removes its key rather than
            // writing an empty string, which the watch can tell apart from an
            // absent one -- so this is the gesture that resets a field to the
            // app's own default, not a mistake to report.
            return Ok(None);
        }
        // The one failure a declared `validationMessage` is not used for. §3.3
        // allows it for any failure on the field, and for every other one it is
        // the better message -- but it states the *shape* a value takes, and
        // "Up to 22 plain characters, copied exactly" is no answer to a box
        // somebody left empty.
        return Err(format!(
            "{} is needed before the app can do anything useful",
            field.label
        ));
    }

    match &field.kind {
        Kind::Bool { .. } => match text {
            "true" => Ok(Some(Value::Flag(true))),
            "false" => Ok(Some(Value::Flag(false))),
            other => Err(field.say(&format!("{other:?} is not a switch position"))),
        },
        Kind::Int { min, max, .. } => {
            // Rejected rather than rounded, as §4.3 requires of a reader: a
            // fractional number in an int field is invalid, not nearly right.
            let value: i32 = text
                .parse()
                .map_err(|_| field.say(&format!("{text:?} is not a whole number")))?;
            if value < *min || value > *max {
                return Err(field.say(&format!("must be between {min} and {max}")));
            }
            Ok(Some(Value::Whole(value)))
        }
        Kind::Float { min, max, .. } => {
            let value: f32 = text
                .parse()
                .map_err(|_| field.say(&format!("{text:?} is not a number")))?;
            if !value.is_finite() {
                return Err(field.say("must be an ordinary number"));
            }
            if value < *min || value > *max {
                return Err(field.say(&format!(
                    "must be between {} and {}",
                    number_text(*min),
                    number_text(*max)
                )));
            }
            Ok(Some(Value::Number(value)))
        }
        Kind::String {
            min_length,
            max_length,
            pattern,
            ..
        } => {
            // Bytes, not characters, because that is what the app's buffer and
            // the file's own limit are measured in: a 16-byte field holds 16
            // ASCII characters but four emoji.
            let bytes = text.len();
            if bytes < *min_length {
                return Err(field.say(&format!(
                    "is {bytes} bytes; the app needs at least {min_length}"
                )));
            }
            // Refused, never trimmed. A shortened id is a *wrong* id, which for
            // the app this was first written for means a barcode that scans as
            // somebody else's number.
            if bytes > *max_length {
                return Err(field.say(&format!(
                    "is {bytes} bytes; the app accepts at most {max_length}"
                )));
            }
            if let Some(bad) = text.chars().find(|c| c.is_control()) {
                return Err(field.say(&format!(
                    "contains {bad:?}, a control character no value may carry"
                )));
            }
            if let Some(expression) = pattern
                && !pattern::matches(expression, text)
            {
                return Err(field.say("is not in the form this field takes"));
            }
            Ok(Some(Value::Text(text.to_owned())))
        }
    }
}

/// Check a declaration before anything acts on it.
///
/// This runs over a third party's manifest and its output decides a file name
/// Kira writes to and the keys it puts in it, so it is deliberately fussier than
/// the shape needs. It is the same rule set as the SDK's
/// `validate_app_config.py`, which the app's own CI runs, plus the two things
/// only the desktop end knows: that a name may resolve to a device rather than a
/// file, and that a host may create a name other than the one it was given.
///
/// # Errors
/// Returns the first reason the declaration cannot be used.
pub fn check_spec(spec: &Spec) -> Result<(), Problem> {
    check_file_name(&spec.file)?;

    if spec.fields.is_empty() {
        return Err("declares no fields, so there is nothing to fill in".to_owned());
    }
    if spec.fields.len() > MAX_FIELDS {
        return Err(format!(
            "declares {} fields; the SDK allows {MAX_FIELDS}",
            spec.fields.len()
        ));
    }

    let mut seen: Vec<String> = Vec::new();
    for field in &spec.fields {
        check_id(&field.id)?;
        // Compared case-insensitively: two ids differing only in case are a bug
        // waiting to happen, and the SDK refuses them for the same reason.
        let folded = field.id.to_ascii_lowercase();
        if seen.contains(&folded) {
            return Err(format!("two fields are both called {}", field.id));
        }
        seen.push(folded);

        check_text(&field.label, "label", MAX_LABEL_CHARS, &field.id)?;
        check_text(
            &field.description,
            "description",
            MAX_DESCRIPTION_CHARS,
            &field.id,
        )?;
        if let Some(message) = &field.validation_message {
            check_text(message, "validationMessage", MAX_MESSAGE_CHARS, &field.id)?;
        }
        if let Some(unit) = field.unit() {
            check_text(unit, "unit", MAX_UNIT_CHARS, &field.id)?;
        }
        check_kind(field)?;
    }

    // Every id present, each holding the longest value it could: a declaration
    // whose answers cannot fit in a file the app will read is broken, and the
    // owner should not find that out with a half-written file on a watch.
    let worst = worst_case_bytes(spec);
    if worst > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "these fields could produce a file of up to {worst} bytes, over the \
             {MAX_DOCUMENT_BYTES}-byte limit the app can read"
        ));
    }
    Ok(())
}

/// Assemble the values file.
///
/// `values` is what the page holds per field id; a field with no entry, or an
/// empty one, has not been answered.
///
/// Which keys are written is §4.4's rule rather than "all of them": a required
/// field is always present, and an optional one is written only when its answer
/// differs from the app's own default, so that `has()` on the watch keeps
/// meaning "the owner chose this" and clearing a field resets it. An optional
/// answer that matches the default is therefore absent from the file, which
/// leaves the app reading exactly the same value.
///
/// # Errors
/// If the declaration is unusable, a required field is unanswered, an answer is
/// rejected, or the result would exceed [`MAX_DOCUMENT_BYTES`].
pub fn document(spec: &Spec, values: &BTreeMap<String, String>) -> Result<String, Problem> {
    check_spec(spec)?;

    let mut written: Vec<(&str, Value)> = Vec::new();
    for field in &spec.fields {
        let text = values.get(&field.id).map_or("", String::as_str);
        let Some(value) = check_value(field, text)? else {
            continue;
        };
        if field.required || value != field.kind.default_value() {
            written.push((field.id.as_str(), value));
        }
    }

    let mut out = format!("{{\n  \"schema\": {VALUES_SCHEMA},\n  \"values\": {{");
    for (at, (id, value)) in written.iter().enumerate() {
        if at > 0 {
            out.push(',');
        }
        let _ = write!(out, "\n    \"{id}\": {}", value_text(value));
    }
    if !written.is_empty() {
        out.push_str("\n  ");
    }
    out.push_str("}\n}\n");

    if out.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "the filled-in file would be {} bytes, over the {MAX_DOCUMENT_BYTES} limit",
            out.len()
        ));
    }
    Ok(out)
}

/// One value as JSON.
fn value_text(value: &Value) -> String {
    match value {
        Value::Flag(flag) => flag.to_string(),
        Value::Whole(number) => number.to_string(),
        Value::Number(number) => number_text(*number),
        // Escaping only what JSON requires, and nothing else: §9.6 forbids
        // `\uXXXX`-escaping ordinary text, because the 8 KB budget is worked out
        // on the assumption that a byte of UTF-8 stays a byte. Control
        // characters were refused when the value was checked, so `"` and `\`
        // are the whole of it.
        Value::Text(text) => {
            let mut out = String::with_capacity(text.len() + 2);
            out.push('"');
            for c in text.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
            out
        }
    }
}

/// Upper bound on the file this declaration could produce.
///
/// The SDK's own accounting, so that a manifest its tooling passed is not
/// refused here: every id present, each holding the longest value a conforming
/// writer could emit, and a string byte costing two in case every one of them
/// needs an escape.
fn worst_case_bytes(spec: &Spec) -> usize {
    let mut size = "{\"schema\":1,\"values\":{}}".len();
    for field in &spec.fields {
        size += field.id.len() + 4; // "id":
        size += match &field.kind {
            Kind::String { max_length, .. } => 2 + max_length * 2,
            Kind::Bool { .. } => "false".len(),
            Kind::Int { .. } => MAX_INT_TEXT,
            Kind::Float { .. } => MAX_FLOAT_TEXT,
        };
        size += 1; // comma
    }
    size
}

/// Check one of a field's human-readable strings.
fn check_text(value: &str, key: &str, limit: usize, id: &str) -> Result<(), Problem> {
    if value.trim().is_empty() {
        return Err(format!("{key} of {id} is empty"));
    }
    let length = value.chars().count();
    if length > limit {
        return Err(format!(
            "{key} of {id} is {length} characters; the SDK allows {limit}"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{key} of {id} is not printable text"));
    }
    Ok(())
}

/// Check the bounds and default that belong to a field's type.
fn check_kind(field: &Field) -> Result<(), Problem> {
    let id = &field.id;
    match &field.kind {
        Kind::Bool { .. } => Ok(()),
        Kind::Int {
            default, min, max, ..
        } => {
            if max < min {
                return Err(format!("max of {id} is below its min"));
            }
            if default < min || default > max {
                return Err(format!("default of {id} is outside its own min..max"));
            }
            Ok(())
        }
        Kind::Float {
            default, min, max, ..
        } => {
            if !min.is_finite() || !max.is_finite() {
                return Err(format!("min and max of {id} must be ordinary numbers"));
            }
            if max < min {
                return Err(format!("max of {id} is below its min"));
            }
            if !default.is_finite() {
                return Err(format!("default of {id} must be an ordinary number"));
            }
            if default < min || default > max {
                return Err(format!("default of {id} is outside its own min..max"));
            }
            Ok(())
        }
        Kind::String {
            default,
            min_length,
            max_length,
            pattern,
        } => {
            if *max_length == 0 || *max_length > MAX_STRING_BYTES {
                return Err(format!(
                    "maxLength of {id} must be 1..={MAX_STRING_BYTES} UTF-8 bytes"
                ));
            }
            if min_length > max_length {
                return Err(format!("minLength of {id} is above its maxLength"));
            }
            if let Some(expression) = pattern {
                pattern::check(expression).map_err(|why| format!("pattern of {id}: {why}"))?;
            }
            let bytes = default.len();
            if bytes > *max_length {
                return Err(format!(
                    "default of {id} is {bytes} bytes; its maxLength is {max_length}"
                ));
            }
            if bytes < *min_length {
                return Err(format!(
                    "default of {id} is {bytes} bytes; its minLength is {min_length}"
                ));
            }
            if let Some(expression) = pattern
                && !pattern::matches(expression, default)
            {
                return Err(format!("default of {id} does not match its own pattern"));
            }
            Ok(())
        }
    }
}

/// Reject an id that is not what §2.3 allows.
fn check_id(id: &str) -> Result<(), Problem> {
    let mut chars = id.chars();
    let ok = matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && id.chars().count() <= MAX_ID_CHARS;
    if ok {
        Ok(())
    } else {
        Err(format!(
            "field id {id:?} must start with a lower-case letter and hold only letters, \
             digits and underscores, up to {MAX_ID_CHARS} characters"
        ))
    }
}

/// Reject anything that is not a plain `.json` file name in the app's folder.
fn check_file_name(name: &str) -> Result<(), Problem> {
    // The SDK's own pattern: ^[A-Za-z0-9][A-Za-z0-9_.-]{0,57}\.json$. Spelled
    // out rather than compiled, since this crate carries no regex engine for
    // anything but a declared field pattern. The suffix is compared without
    // regard to case because the volume it lands on is a FAT one.
    let stem = name
        .len()
        .checked_sub(".json".len())
        .and_then(|at| {
            let (stem, suffix) = name.split_at_checked(at)?;
            suffix.eq_ignore_ascii_case(".json").then_some(stem)
        })
        .filter(|stem| {
            !stem.is_empty()
                && stem.starts_with(|c: char| c.is_ascii_alphanumeric())
                && stem.chars().count() <= 58
                && stem
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        });
    if stem.is_none() {
        return Err(format!(
            "file name {name:?} must be a bare name ending in .json, with no path of its own"
        ));
    }
    let lower = name.to_ascii_lowercase();
    // Reserved: that is the package metadata, which never reaches the watch.
    // Compared case-insensitively, because the watch's FAT volume is too.
    if lower == RESERVED_FILE_NAME {
        return Err(format!(
            "file name {name:?} is reserved for the package metadata, which is never \
             copied to the watch"
        ));
    }
    // Windows drops a trailing dot or space when it creates a file, so a name
    // like this is a request for a *different* name than the one just checked.
    // Unreachable through the pattern above, and kept because the rule belongs
    // to the name rather than to the pattern that happens to exclude it.
    if crate::fat::is_trimmed_by_host(name) {
        return Err(format!(
            "file name {name:?} ends in a dot or a space, which some systems drop when \
             they create the file"
        ));
    }
    // A folder was already refused one of these; a file never was. On Windows
    // the write goes to the device rather than the volume, so the file the app
    // looks for is simply never there -- and `nul.json` passes every rule above.
    if crate::fat::is_reserved_device(name) {
        return Err(format!(
            "file name {name:?} is a reserved device name, which does not write to a file"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_field(id: &str, max_length: usize) -> Field {
        Field {
            id: id.to_owned(),
            label: "Athlete id".to_owned(),
            description: "The characters printed under the barcode on your card.".to_owned(),
            required: false,
            validation_message: None,
            kind: Kind::String {
                default: String::new(),
                min_length: 0,
                max_length,
                pattern: None,
            },
        }
    }

    fn spec(fields: Vec<Field>) -> Spec {
        Spec {
            file: "input.json".to_owned(),
            fields,
        }
    }

    fn answers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn the_document_is_the_envelope_the_sdk_specifies() {
        let text = document(
            &spec(vec![text_field("id1", 16)]),
            &answers(&[("id1", "A1234567")]),
        )
        .expect("valid");
        assert_eq!(
            text,
            "{\n  \"schema\": 1,\n  \"values\": {\n    \"id1\": \"A1234567\"\n  }\n}\n"
        );
    }

    #[test]
    fn every_type_writes_its_own_json() {
        let fields = vec![
            Field {
                required: true,
                ..text_field("name", 16)
            },
            Field {
                kind: Kind::Bool { default: false },
                ..text_field("lit", 16)
            },
            Field {
                kind: Kind::Int {
                    default: 0,
                    min: 0,
                    max: 60,
                    unit: Some("min".to_owned()),
                },
                ..text_field("lap", 16)
            },
            Field {
                kind: Kind::Float {
                    default: 0.0,
                    min: -90.0,
                    max: 90.0,
                    unit: Some("deg".to_owned()),
                },
                ..text_field("lat", 16)
            },
        ];
        let text = document(
            &spec(fields),
            &answers(&[
                ("name", "Trailhead"),
                ("lit", "true"),
                ("lap", "5"),
                ("lat", "45.4215"),
            ]),
        )
        .expect("valid");
        assert!(text.contains(r#""name": "Trailhead""#), "{text}");
        assert!(text.contains(r#""lit": true"#), "{text}");
        assert!(text.contains(r#""lap": 5"#), "{text}");
        // Plain decimal: no exponent and no thousands separator, which is
        // §9.6's rule for a writer.
        assert!(text.contains("\"lat\": 45.4215\n"), "{text}");
    }

    #[test]
    fn an_answer_that_matches_the_app_s_default_is_left_out() {
        // §4.4: an optional field the owner has not moved is absent, which is
        // how the app tells "they chose this" from "this is my default".
        let field = Field {
            kind: Kind::Bool { default: false },
            ..text_field("recordImu", 16)
        };
        let text = document(
            &spec(vec![field.clone()]),
            &answers(&[("recordImu", "false")]),
        )
        .expect("valid");
        assert_eq!(text, "{\n  \"schema\": 1,\n  \"values\": {}\n}\n");

        let moved =
            document(&spec(vec![field]), &answers(&[("recordImu", "true")])).expect("valid");
        assert!(moved.contains(r#""recordImu": true"#), "{moved}");
    }

    #[test]
    fn a_required_answer_is_written_even_when_it_matches_the_default() {
        // The owner had to supply it, so the file says what they supplied
        // rather than leaving the app to infer it.
        let field = Field {
            required: true,
            kind: Kind::Int {
                default: 5,
                min: 0,
                max: 8,
                unit: None,
            },
            ..text_field("zones", 16)
        };
        let text = document(&spec(vec![field]), &answers(&[("zones", "5")])).expect("valid");
        assert!(text.contains(r#""zones": 5"#), "{text}");
    }

    #[test]
    fn an_unanswered_optional_field_writes_no_key() {
        let text = document(&spec(vec![text_field("id1", 16)]), &answers(&[])).expect("valid");
        assert_eq!(text, "{\n  \"schema\": 1,\n  \"values\": {}\n}\n");
    }

    #[test]
    fn an_unanswered_required_field_stops_the_file_being_written() {
        let field = Field {
            required: true,
            validation_message: Some("Up to 16 plain characters, copied exactly.".to_owned()),
            ..text_field("id1", 16)
        };
        let err = document(&spec(vec![field]), &answers(&[])).expect_err("needed");
        // Not the declared message: that one states the shape a value takes,
        // which is no answer to a box somebody left empty.
        assert_eq!(
            err,
            "Athlete id is needed before the app can do anything useful"
        );
    }

    #[test]
    fn an_over_long_value_is_refused_rather_than_trimmed() {
        // Trimming would produce a different id that still scans.
        let err = document(
            &spec(vec![text_field("id1", 4)]),
            &answers(&[("id1", "A1234567")]),
        )
        .expect_err("too long");
        assert!(err.contains("at most 4"), "{err}");
    }

    #[test]
    fn length_is_counted_in_bytes_because_that_is_what_the_app_holds() {
        // "мама" is four characters and eight bytes. A buffer sized in bytes
        // is what the app declared, so bytes are what the limit counts: the
        // shorter word fits a six-byte field and the longer one does not.
        assert!(
            document(
                &spec(vec![text_field("id1", 6)]),
                &answers(&[("id1", "мам")])
            )
            .is_ok()
        );
        let err = document(
            &spec(vec![text_field("id1", 6)]),
            &answers(&[("id1", "мама")]),
        )
        .expect_err("eight bytes in a six-byte field");
        assert!(err.contains("is 8 bytes"), "{err}");
    }

    #[test]
    fn a_quote_or_a_backslash_is_escaped_rather_than_refused() {
        // The old convention refused both, to keep the writer free of escapes.
        // The SDK's says to escape what JSON requires and nothing more, and
        // `SDK::AppConfig` unescapes correctly, so a name with an apostrophe or
        // a quote in it is a name somebody can have.
        let text = document(
            &spec(vec![text_field("name1", 24)]),
            &answers(&[("name1", r#"Bob's "gym""#)]),
        )
        .expect("valid");
        assert!(text.contains(r#""name1": "Bob's \"gym\"""#), "{text}");
    }

    #[test]
    fn a_control_character_is_refused() {
        for bad in ["a\tb", "a\nb", "a\u{0}b"] {
            let err = document(
                &spec(vec![text_field("id1", 16)]),
                &answers(&[("id1", bad)]),
            )
            .expect_err("control");
            assert!(err.contains("control character"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_declared_pattern_is_applied_because_the_app_cannot() {
        // `SDK::AppConfig` has no regular-expression engine and never reads
        // `pattern`, so a value that fails it would be one the app acts on.
        let field = Field {
            kind: Kind::String {
                default: "Code128".to_owned(),
                min_length: 0,
                max_length: 8,
                pattern: Some("(?:[Cc][Oo][Dd][Ee]128|[Ii][Tt][Ff])".to_owned()),
            },
            ..text_field("fmt1", 8)
        };
        assert!(document(&spec(vec![field.clone()]), &answers(&[("fmt1", "ITF")])).is_ok());
        let err = document(&spec(vec![field]), &answers(&[("fmt1", "Code129")]))
            .expect_err("not a format");
        assert!(err.contains("form this field takes"), "{err}");
    }

    #[test]
    fn a_declared_message_is_what_a_refusal_says() {
        let field = Field {
            validation_message: Some("Up to 22 plain characters, copied exactly.".to_owned()),
            ..text_field("id1", 4)
        };
        let err =
            document(&spec(vec![field]), &answers(&[("id1", "A1234567")])).expect_err("too long");
        assert!(err.contains("Up to 22 plain characters"), "{err}");
    }

    #[test]
    fn a_number_outside_its_bounds_is_refused_rather_than_clamped() {
        // The app clamps -- it has the bounds compiled in -- but a companion app
        // must never write an invalid value (§9.6), and clamping silently would
        // record a target the owner did not ask for.
        let field = Field {
            kind: Kind::Int {
                default: 0,
                min: 0,
                max: 60,
                unit: Some("min".to_owned()),
            },
            ..text_field("lap", 16)
        };
        let err = document(&spec(vec![field]), &answers(&[("lap", "90")])).expect_err("over");
        assert!(err.contains("between 0 and 60"), "{err}");
    }

    #[test]
    fn a_fractional_answer_to_a_whole_number_field_is_refused() {
        let field = Field {
            kind: Kind::Int {
                default: 0,
                min: 0,
                max: 60,
                unit: None,
            },
            ..text_field("lap", 16)
        };
        let err = document(&spec(vec![field]), &answers(&[("lap", "5.5")])).expect_err("not whole");
        assert!(err.contains("whole number"), "{err}");
    }

    #[test]
    fn a_non_finite_number_never_reaches_a_file() {
        let field = Field {
            kind: Kind::Float {
                default: 0.0,
                min: -90.0,
                max: 90.0,
                unit: None,
            },
            ..text_field("lat", 16)
        };
        for bad in ["NaN", "inf", "-inf"] {
            assert!(
                document(&spec(vec![field.clone()]), &answers(&[("lat", bad)])).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn a_file_name_cannot_escape_the_app_folder() {
        for bad in [
            "../settings.json",
            "sub/dir.json",
            "..",
            ".hidden.json",
            "a\\b.json",
            "",
            "input.txt",
            ".json",
        ] {
            let mut s = spec(vec![text_field("id1", 16)]);
            s.file = bad.to_owned();
            assert!(check_spec(&s).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn the_package_metadata_file_is_not_a_values_file() {
        // It never reaches the watch, and a values file by that name would
        // arrive claiming to be the thing the phone reads the declaration from.
        for bad in ["app-manifest.json", "APP-MANIFEST.JSON"] {
            let mut s = spec(vec![text_field("id1", 16)]);
            s.file = bad.to_owned();
            assert!(check_spec(&s).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_values_file_cannot_be_named_after_a_device() {
        // On Windows the write lands on the device rather than the volume, so
        // the app looks for a file that was never created. The extension does
        // not help: `nul.json` resolves to NUL too.
        for bad in ["nul.json", "CON.json", "aux.json", "com1.json"] {
            let mut s = spec(vec![text_field("id1", 16)]);
            s.file = bad.to_owned();
            assert!(check_spec(&s).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn an_id_that_is_not_the_sdk_s_shape_is_refused() {
        for bad in ["Id1", "1id", "id-1", "values.id", "", "__proto__x."] {
            let s = spec(vec![text_field(bad, 16)]);
            assert!(check_spec(&s).is_err(), "accepted {bad:?}");
        }
        // Long enough to be refused on length alone.
        let s = spec(vec![text_field(&format!("a{}", "b".repeat(32)), 16)]);
        assert!(check_spec(&s).is_err());
    }

    #[test]
    fn two_ids_differing_only_in_case_are_refused() {
        let s = spec(vec![
            text_field("waypointName", 16),
            text_field("waypointname", 16),
        ]);
        let err = check_spec(&s).expect_err("duplicate");
        assert!(err.contains("both called"), "{err}");
    }

    #[test]
    fn a_declaration_with_nothing_to_fill_in_is_refused() {
        assert!(check_spec(&spec(vec![])).is_err());
    }

    #[test]
    fn more_fields_than_the_app_can_hold_are_refused() {
        let fields: Vec<Field> = (0..=MAX_FIELDS)
            .map(|i| text_field(&format!("f{i}"), 4))
            .collect();
        let err = check_spec(&spec(fields)).expect_err("too many");
        assert!(err.contains("allows 32"), "{err}");
    }

    #[test]
    fn a_default_that_breaks_its_own_field_is_refused() {
        let too_long = Field {
            kind: Kind::String {
                default: "abcdefghij".to_owned(),
                min_length: 0,
                max_length: 4,
                pattern: None,
            },
            ..text_field("id1", 4)
        };
        assert!(check_spec(&spec(vec![too_long])).is_err());

        let outside = Field {
            kind: Kind::Int {
                default: 99,
                min: 0,
                max: 60,
                unit: None,
            },
            ..text_field("lap", 4)
        };
        assert!(check_spec(&spec(vec![outside])).is_err());

        let unmatchable = Field {
            kind: Kind::String {
                default: "nope".to_owned(),
                min_length: 0,
                max_length: 8,
                pattern: Some("[0-9]+".to_owned()),
            },
            ..text_field("id1", 8)
        };
        assert!(check_spec(&spec(vec![unmatchable])).is_err());
    }

    #[test]
    fn a_pattern_outside_the_dialect_is_refused_by_the_declaration_check() {
        let field = Field {
            kind: Kind::String {
                default: "a".to_owned(),
                min_length: 0,
                max_length: 8,
                pattern: Some("^(a+)+$".to_owned()),
            },
            ..text_field("id1", 8)
        };
        let err = check_spec(&spec(vec![field])).expect_err("dialect");
        assert!(err.contains("pattern of id1"), "{err}");
    }

    #[test]
    fn a_declaration_whose_answers_could_not_fit_the_file_is_refused() {
        // The app holds the whole file to parse it and refuses one over 8 KB, so
        // a declaration that could produce a longer one is broken as declared.
        let fields: Vec<Field> = (0..32)
            .map(|i| text_field(&format!("f{i}"), MAX_STRING_BYTES))
            .collect();
        let err = check_spec(&spec(fields)).expect_err("too big");
        assert!(err.contains("over the 8192-byte limit"), "{err}");
    }

    #[test]
    fn an_oversized_document_is_refused_before_it_is_written() {
        let long = "x".repeat(MAX_STRING_BYTES);
        let fields: Vec<Field> = (0..30)
            .map(|i| text_field(&format!("f{i}"), MAX_STRING_BYTES))
            .collect();
        let filled: BTreeMap<String, String> =
            (0..30).map(|i| (format!("f{i}"), long.clone())).collect();
        // The declaration itself is within the worst case, and the answers are
        // plain ASCII, so this one is about the assembled bytes.
        let s = spec(fields);
        assert!(check_spec(&s).is_ok());
        let text = document(&s, &filled).expect("fits");
        assert!(text.len() <= MAX_DOCUMENT_BYTES);
    }

    #[test]
    fn the_declaration_round_trips_through_the_manifest_s_own_json() {
        // What the catalogue carries is the app's declaration, not a
        // translation: the keys are `app-manifest.json`'s and the types are
        // tagged the way it tags them.
        let text = r#"{
          "file": "app_config.json",
          "fields": [
            {"id": "waypointName", "type": "string", "label": "Waypoint name",
             "description": "Shown on the watch while you navigate.",
             "default": "Waypoint", "minLength": 1, "maxLength": 16,
             "pattern": "[A-Za-z0-9 ]+",
             "validationMessage": "Up to 16 letters, digits and spaces."},
            {"id": "targetLatitude", "type": "float", "label": "Target latitude",
             "description": "Latitude in decimal degrees.", "default": 51.5072,
             "min": -90.0, "max": 90.0, "unit": "deg", "required": true},
            {"id": "arrivalRadiusM", "type": "int", "label": "Arrival radius",
             "description": "How close counts as arrived.", "default": 25,
             "min": 5, "max": 500, "unit": "m"},
            {"id": "vibrateOnArrival", "type": "bool", "label": "Vibrate",
             "description": "Buzz once on arrival.", "default": true}
          ]
        }"#;
        let parsed: Spec = serde_json::from_str(text).expect("parses");
        check_spec(&parsed).expect("the SDK's own worked example is valid");
        assert_eq!(parsed.fields.len(), 4);
        assert_eq!(parsed.fields[0].kind.type_name(), "string");
        assert_eq!(parsed.fields[1].unit(), Some("deg"));
        assert!(parsed.fields[1].required);
        assert_eq!(parsed.fields[3].default_text(), "true");

        let json = serde_json::to_string(&parsed).expect("serialises");
        assert!(json.contains(r#""type":"float""#), "{json}");
        assert!(json.contains(r#""maxLength":16"#), "{json}");
        let back: Spec = serde_json::from_str(&json).expect("round trips");
        assert_eq!(back, parsed);
    }

    #[test]
    fn a_field_that_leaves_out_what_its_type_needs_does_not_parse() {
        // An int with no bounds, a string with no maxLength: the shape carries
        // the requirement, so this is refused before any check runs.
        for bad in [
            r#"{"id":"a","type":"int","label":"A","description":"d","default":1}"#,
            r#"{"id":"a","type":"string","label":"A","description":"d","default":""}"#,
            r#"{"id":"a","type":"bool","label":"A","description":"d"}"#,
            r#"{"id":"a","type":"colour","label":"A","description":"d","default":"red"}"#,
        ] {
            assert!(serde_json::from_str::<Field>(bad).is_err(), "parsed {bad}");
        }
    }

    #[test]
    fn required_stays_out_of_the_catalogue_when_it_is_not_set() {
        let field = text_field("id1", 16);
        let json = serde_json::to_string(&field).expect("serialises");
        assert!(!json.contains("required"), "{json}");
        assert!(!json.contains("minLength"), "{json}");
        assert!(!json.contains("pattern"), "{json}");
    }
}
