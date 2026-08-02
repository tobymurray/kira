//! A file an app reads its user-specific settings from, and the form that fills
//! it in.
//!
//! The watch has four buttons and no keyboard, so a value only its owner knows —
//! a parkrun athlete id, a transit pass, an account token — cannot be entered on
//! the device, and the SDK offers no way to send one in. What it does offer is a
//! USB mass-storage volume: `Apps/<Folder>/` is writable from any desktop, and it
//! is where an app's own relative paths resolve. So the file goes there, and Kira
//! is already the page holding a handle to that directory.
//!
//! **Kira invents no part of the format.** The file name, the schema number and
//! every key come from the app's manifest; all this module does is assemble the
//! document and refuse values the app could not read back. That matters because
//! the convention itself is nobody's standard yet — the SDK ships
//! `SDK::Variant::Config`, a bounded schema-versioned JSON reader with exactly
//! this shape, but only for configs the platform writes. If UNA ever blesses a
//! source a user can write, an app changes its manifest and Kira needs no edit.
//!
//! Unlike everything else on a card, a config declaration cannot be derived from
//! the binary — nothing in a `.uapp` says what it reads. It is an assertion by
//! the submitter, and the only assertion Kira acts on rather than merely displays,
//! which is why the checks below are stricter than a schema needs to be.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Largest document Kira will assemble, in bytes.
///
/// A provisioning file is tens of bytes. The ceiling exists because the app on
/// the other end has one too — the reference reader refuses anything over 4 KB
/// before it allocates — and writing a file an app will only reject is worse
/// than refusing in the form, where there is somewhere to show why.
pub const MAX_DOCUMENT_BYTES: usize = 2048;

/// Longest value a field may accept.
pub const MAX_FIELD_LENGTH: usize = 256;

/// One value the user supplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Field {
    /// Dot-separated location in the document, e.g. `values.id`.
    ///
    /// The app owns this. Kira builds whatever nesting it describes rather than
    /// imposing a shape of its own.
    pub path: String,
    /// Label for the input.
    pub title: String,
    /// One line under the label, when the title is not enough on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// Longest value the app will accept.
    pub max_length: usize,
}

/// What an app says it reads, and from where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Spec {
    /// File name within `Apps/<Folder>/`. A bare name: never a path.
    pub file: String,
    /// Written as the document's top-level `schema`.
    ///
    /// Apps that gate on an exact major — as `SDK::Variant::Config` does, and as
    /// the reference reader copies — need this to match or they fall back to
    /// their defaults, so it belongs to the app rather than to Kira.
    pub schema: u32,
    /// Every value the user is asked for, in the order they are shown.
    pub fields: Vec<Field>,
}

/// Why a value or a declaration cannot be used.
pub type Problem = String;

/// Whether a character can make the round trip to an app and back out intact.
///
/// Printable ASCII only, less backslash and double quote. Those two are dropped
/// for the same reason rather than for JSON's: a reader built on coreJSON is
/// handed the raw slice with escapes left undecoded, so an escaped character
/// arrives as the literal characters of its escape sequence. A value that cannot
/// survive the trip should not be offered, and refusing here means the assembled
/// document needs no escaping at all — which is the property that makes writing
/// it safe.
#[must_use]
pub fn is_transportable(c: char) -> bool {
    matches!(c, ' '..='~') && c != '\\' && c != '"'
}

/// Check one value against its field.
///
/// Rejects rather than trims: a shortened id is a *wrong* id, and for the app
/// this was written for that means a barcode scanning as somebody else's number.
///
/// # Errors
/// Returns the reason, phrased for whoever has to retype the value.
pub fn check_value(field: &Field, value: &str) -> Result<(), Problem> {
    if value.is_empty() {
        return Err(format!("{} is empty", field.title));
    }
    let length = value.chars().count();
    if length > field.max_length {
        return Err(format!(
            "{} is {length} characters; the app accepts at most {}",
            field.title, field.max_length
        ));
    }
    if let Some(bad) = value.chars().find(|c| !is_transportable(*c)) {
        return Err(format!(
            "{} contains {bad:?}, which the app cannot read back",
            field.title
        ));
    }
    Ok(())
}

/// Check a declaration before anything acts on it.
///
/// This runs over a third party's manifest, and its output decides a filename
/// Kira writes to and the keys it writes. Treated accordingly.
///
/// # Errors
/// Returns the first reason the declaration cannot be used.
pub fn check_spec(spec: &Spec) -> Result<(), Problem> {
    check_file_name(&spec.file)?;

    if spec.fields.is_empty() {
        return Err("declares no fields, so there is nothing to fill in".to_owned());
    }

    let mut seen: Vec<&str> = Vec::new();
    for field in &spec.fields {
        check_path(&field.path)?;
        if field.title.trim().is_empty() {
            return Err(format!("field {} has no title", field.path));
        }
        if !field.title.chars().all(is_renderable) {
            return Err(format!("title of {} is not printable text", field.path));
        }
        if let Some(help) = &field.help
            && !help.chars().all(is_renderable)
        {
            return Err(format!("help for {} is not printable text", field.path));
        }
        if field.max_length == 0 || field.max_length > MAX_FIELD_LENGTH {
            return Err(format!(
                "maxLength of {} must be 1..={MAX_FIELD_LENGTH}",
                field.path
            ));
        }
        // A path that is a prefix of another would have to be both a string and
        // an object in the same document.
        for other in &seen {
            if field.path == *other {
                return Err(format!("two fields both write {}", field.path));
            }
            if is_prefix_path(&field.path, other) || is_prefix_path(other, &field.path) {
                return Err(format!("{} and {other} cannot both exist", field.path));
            }
        }
        seen.push(&field.path);
    }
    Ok(())
}

/// Assemble the document, with every field filled in.
///
/// Every value is checked first, so the result contains no character needing an
/// escape. That is not an optimisation — it is why this can concatenate strings
/// instead of depending on a JSON encoder the browser already has and this crate
/// deliberately does not ship.
///
/// # Errors
/// If the declaration is unusable, a field has no value, a value is rejected, or
/// the result would exceed [`MAX_DOCUMENT_BYTES`].
pub fn document(spec: &Spec, values: &BTreeMap<String, String>) -> Result<String, Problem> {
    check_spec(spec)?;

    let mut root = Node::default();
    for field in &spec.fields {
        let value = values
            .get(&field.path)
            .ok_or_else(|| format!("no value given for {}", field.title))?;
        check_value(field, value)?;
        root.insert(&field.path, value);
    }

    let mut out = String::from("{\n  \"schema\": ");
    out.push_str(&spec.schema.to_string());
    for (key, child) in &root.children {
        out.push_str(",\n");
        child.write(key, 1, &mut out);
    }
    out.push_str("\n}\n");

    if out.len() > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "the filled-in file would be {} bytes, over the {MAX_DOCUMENT_BYTES} limit",
            out.len()
        ));
    }
    debug_assert!(
        out.chars().filter(|c| *c == '"').count() % 2 == 0,
        "values are screened, so every quote is structural"
    );
    Ok(out)
}

/// A printable character for a label: anything that is not a control code.
fn is_renderable(c: char) -> bool {
    !c.is_control()
}

/// Reject anything that is not a plain file name sitting in the app's folder.
fn check_file_name(name: &str) -> Result<(), Problem> {
    if name.is_empty() {
        return Err("declares no file name".to_owned());
    }
    if name.len() > 64 {
        return Err(format!("file name {name:?} is too long"));
    }
    if name == "." || name == ".." || name.starts_with('.') {
        return Err(format!("file name {name:?} is not a plain name"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!(
            "file name {name:?} may use only letters, digits, dot, dash and underscore"
        ));
    }
    // The watch loads the first .uapp it finds in a folder, so a config file
    // claiming that extension could displace the app itself.
    if name.to_ascii_lowercase().ends_with(".uapp") {
        return Err(format!("file name {name:?} would look like an app binary"));
    }
    Ok(())
}

/// Reject anything that is not dot-separated plain segments.
fn check_path(path: &str) -> Result<(), Problem> {
    if path.is_empty() {
        return Err("a field has an empty path".to_owned());
    }
    if path.len() > 128 {
        return Err(format!("path {path:?} is too long"));
    }
    for segment in path.split('.') {
        if segment.is_empty() {
            return Err(format!("path {path:?} has an empty segment"));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(format!(
                "path {path:?} may use only letters, digits, dash and underscore between dots"
            ));
        }
    }
    Ok(())
}

/// Whether `outer` names an ancestor of `inner`, e.g. `values` of `values.id`.
fn is_prefix_path(outer: &str, inner: &str) -> bool {
    inner
        .strip_prefix(outer)
        .is_some_and(|r| r.starts_with('.'))
}

/// One level of the document being assembled.
#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    value: Option<String>,
}

impl Node {
    fn insert(&mut self, path: &str, value: &str) {
        match path.split_once('.') {
            Some((head, rest)) => self
                .children
                .entry(head.to_owned())
                .or_default()
                .insert(rest, value),
            None => {
                self.children.entry(path.to_owned()).or_default().value = Some(value.to_owned());
            }
        }
    }

    fn write(&self, key: &str, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        if let Some(value) = &self.value {
            let _ = write!(out, "{pad}\"{key}\": \"{value}\"");
        } else {
            let _ = writeln!(out, "{pad}\"{key}\": {{");
            for (i, (child_key, child)) in self.children.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                child.write(child_key, depth + 1, out);
            }
            out.push('\n');
            let _ = write!(out, "{pad}}}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(path: &str, max_length: usize) -> Field {
        Field {
            path: path.to_owned(),
            title: "Athlete id".to_owned(),
            help: None,
            max_length,
        }
    }

    fn spec(fields: Vec<Field>) -> Spec {
        Spec {
            file: "input.json".to_owned(),
            schema: 1,
            fields,
        }
    }

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_document_nests_by_the_path_the_app_declared() {
        let text = document(
            &spec(vec![field("values.id", 16)]),
            &values(&[("values.id", "A1234567")]),
        )
        .expect("valid");
        assert_eq!(
            text,
            "{\n  \"schema\": 1,\n  \"values\": {\n    \"id\": \"A1234567\"\n  }\n}\n"
        );
    }

    #[test]
    fn sibling_fields_share_a_parent_object() {
        let text = document(
            &spec(vec![field("values.id", 16), field("values.club", 16)]),
            &values(&[("values.id", "A1"), ("values.club", "B2")]),
        )
        .expect("valid");
        assert_eq!(text.matches("\"values\"").count(), 1);
        assert!(text.contains("\"club\": \"B2\""));
        assert!(text.contains("\"id\": \"A1\""));
    }

    #[test]
    fn a_top_level_field_needs_no_nesting() {
        let text = document(&spec(vec![field("id", 16)]), &values(&[("id", "A1")])).expect("valid");
        assert_eq!(text, "{\n  \"schema\": 1,\n  \"id\": \"A1\"\n}\n");
    }

    #[test]
    fn an_over_long_value_is_refused_rather_than_trimmed() {
        // Trimming would produce a different id that still scans.
        let err = document(
            &spec(vec![field("values.id", 4)]),
            &values(&[("values.id", "A1234567")]),
        )
        .expect_err("too long");
        assert!(err.contains("at most 4"), "{err}");
    }

    #[test]
    fn characters_the_app_cannot_read_back_are_refused() {
        for bad in ["a\\b", "a\"b", "a\tb", "café"] {
            let err = document(
                &spec(vec![field("values.id", 16)]),
                &values(&[("values.id", bad)]),
            )
            .expect_err("untransportable");
            assert!(err.contains("cannot read back"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_document_never_needs_an_escape() {
        // The charset rule is what lets this be assembled by concatenation, so
        // it is worth asserting rather than assuming.
        let text = document(
            &spec(vec![field("values.id", 40)]),
            &values(&[("values.id", "A1 !#$%&'()*+,-./:;<=>?@[]^_`{|}~")]),
        )
        .expect("valid");
        assert!(!text.contains('\\'));
    }

    #[test]
    fn a_missing_value_is_not_silently_omitted() {
        let err = document(&spec(vec![field("values.id", 16)]), &values(&[])).expect_err("missing");
        assert!(err.contains("no value given"), "{err}");
    }

    #[test]
    fn a_file_name_cannot_escape_the_app_folder() {
        for bad in [
            "../settings.json",
            "sub/dir.json",
            "..",
            ".hidden",
            "a\\b.json",
            "",
        ] {
            let mut s = spec(vec![field("values.id", 16)]);
            s.file = bad.to_owned();
            assert!(check_spec(&s).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_config_file_cannot_masquerade_as_the_app_binary() {
        // The watch loads the first .uapp in a folder, so this one could boot.
        let mut s = spec(vec![field("values.id", 16)]);
        s.file = "Config.UAPP".to_owned();
        assert!(check_spec(&s).is_err());
    }

    #[test]
    fn a_path_cannot_carry_json_of_its_own() {
        for bad in ["values.\"id\"", "values..id", ".id", "id.", "va lues.id"] {
            let s = spec(vec![field(bad, 16)]);
            assert!(check_spec(&s).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_field_cannot_be_nested_inside_another() {
        let s = spec(vec![field("values", 16), field("values.id", 16)]);
        let err = check_spec(&s).expect_err("contradictory");
        assert!(err.contains("cannot both exist"), "{err}");
    }

    #[test]
    fn two_fields_cannot_write_the_same_place() {
        let s = spec(vec![field("values.id", 16), field("values.id", 8)]);
        assert!(check_spec(&s).is_err());
    }

    #[test]
    fn a_declaration_with_nothing_to_fill_in_is_refused() {
        assert!(check_spec(&spec(vec![])).is_err());
    }

    #[test]
    fn a_label_cannot_carry_control_characters() {
        let mut s = spec(vec![field("values.id", 16)]);
        s.fields[0].title = "Athlete\nid".to_owned();
        assert!(check_spec(&s).is_err());
    }

    #[test]
    fn max_length_is_bounded_by_what_a_watch_could_use() {
        let s = spec(vec![field("values.id", MAX_FIELD_LENGTH + 1)]);
        assert!(check_spec(&s).is_err());
        assert!(check_spec(&spec(vec![field("values.id", 0)])).is_err());
    }

    #[test]
    fn an_oversized_document_is_refused_before_it_is_written() {
        let long = "x".repeat(MAX_FIELD_LENGTH);
        let fields: Vec<Field> = (0..16)
            .map(|i| field(&format!("values.f{i}"), MAX_FIELD_LENGTH))
            .collect();
        let pairs: Vec<(String, String)> = (0..16)
            .map(|i| (format!("values.f{i}"), long.clone()))
            .collect();
        let map: BTreeMap<String, String> = pairs.into_iter().collect();
        let err = document(&spec(fields), &map).expect_err("too big");
        assert!(err.contains("over the"), "{err}");
    }
}
