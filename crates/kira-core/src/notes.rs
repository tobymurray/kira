//! Making upstream's release bodies readable.
//!
//! A release body is GitHub's generated "What's Changed" list: one bullet per
//! merged pull request, and — because the SDK uses Conventional Commits — each
//! bullet carries a type and usually a scope. Most of them are about the
//! repository rather than the watch: documentation, the desktop simulator, build
//! tooling. Presented as one wall of text, the handful of lines that change what
//! an app does are indistinguishable from the rest.
//!
//! So the lines are parsed and split into what can reach a watch and what cannot.
//! **Nothing is discarded**: an unrecognised line is kept verbatim, and the split
//! is deliberately biased towards "this ships", because wrongly demoting a real
//! app fix is the harmful error and wrongly promoting a docs change is not. The
//! sole exception reads the description rather than the scope, and only to catch
//! simulator work the SDK files under an app's name. What
//! actually changed in a release is still decided by comparing binaries — see
//! [`crate::catalog::changed_in`] — and this only orders the prose around that.

use serde::{Deserialize, Serialize};

/// Conventional-commit types that never affect a binary on the watch.
const OFF_DEVICE_KINDS: &[&str] = &["docs", "ci", "build", "chore", "test", "style"];

/// Scopes that name something other than the code an app is built from.
const OFF_DEVICE_SCOPES: &[&str] = &[
    "doc",
    "docs",
    "sim",
    "sims",
    "simulator",
    "simulators",
    "tool",
    "tools",
    "ci",
    "workflow",
    "workflows",
    "packer",
    "script",
    "scripts",
    "vscode",
    "editor",
    "readme",
    "repo",
    "deps",
    "release",
];

/// Words that name the desktop simulator as the thing being changed.
///
/// Needed because the SDK scopes simulator work to the app it belongs to —
/// `fix(hrmonitor): make the GCC/Linux simulator build` cannot alter a watch, but
/// its scope says `hrmonitor`. Matched on whole words only, so "simulate" and
/// "similar" do not qualify.
const SIMULATOR_WORDS: &[&str] = &["simulator", "simulators", "sim", "sims", "msvc", "touchgfx"];

/// One entry from a release body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// Conventional-commit type, e.g. `fix`. Empty when the line is not one.
    pub kind: String,
    /// Scopes from `type(a,b):`, in the order written.
    pub scopes: Vec<String>,
    /// The description, with the type, author and link removed.
    pub subject: String,
    /// Whether the line was marked breaking with `!`.
    pub breaking: bool,
    /// GitHub handle credited by the generated notes.
    pub author: Option<String>,
    /// Pull request number, when the line links to one.
    pub pr: Option<u32>,
    /// Link to the pull request.
    pub url: Option<String>,
}

impl Change {
    /// Whether this change could alter a binary Kira installs.
    ///
    /// Biased towards `true`: only a recognised off-device type, or a line whose
    /// every scope names something off-device, is demoted. Anything unparsed or
    /// unfamiliar counts as shipping.
    #[must_use]
    pub fn ships(&self) -> bool {
        if OFF_DEVICE_KINDS.contains(&self.kind.as_str()) {
            return false;
        }
        if self.names_the_simulator() {
            return false;
        }
        if self.scopes.is_empty() {
            return true;
        }
        !self
            .scopes
            .iter()
            .all(|scope| OFF_DEVICE_SCOPES.contains(&scope.as_str()))
    }

    /// Whether the description itself says the simulator is what changed.
    ///
    /// The one place a subject overrides its scope. Kept to whole-word matches on
    /// a short list, because reading intent out of prose is guesswork and the
    /// cost of guessing wrong is a real fix landing in the collapsed section.
    fn names_the_simulator(&self) -> bool {
        self.subject
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| SIMULATOR_WORDS.contains(&word.to_ascii_lowercase().as_str()))
    }

    /// The change as one line, without the type prefix.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        if self.breaking {
            out.push_str("breaking: ");
        }
        if !self.scopes.is_empty() {
            out.push_str(&self.scopes.join(", "));
            out.push_str(" — ");
        }
        out.push_str(&self.subject);
        out
    }
}

/// A release body, split by what it can affect.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notes {
    /// Changes that could reach a watch.
    pub shipped: Vec<Change>,
    /// Documentation, simulator and tooling changes.
    pub other: Vec<Change>,
    /// Lines that are not bullets at all, kept verbatim and in order.
    pub prose: Vec<String>,
}

impl Notes {
    /// Whether anything at all was recognised.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shipped.is_empty() && self.other.is_empty() && self.prose.is_empty()
    }
}

/// Split one bullet's trailing `by @author in <url>` from its description.
///
/// GitHub appends both; the URL is what yields the pull request number.
fn split_attribution(text: &str) -> (&str, Option<String>, Option<String>) {
    let (head, url) = match text.rfind(" in http") {
        Some(at) => (&text[..at], Some(text[at + 4..].trim().to_owned())),
        None => (text, None),
    };
    let (head, author) = match head.rfind(" by @") {
        Some(at) => (&head[..at], Some(head[at + 5..].trim().to_owned())),
        None => (head, None),
    };
    (head.trim(), author, url)
}

/// The trailing path segment of a pull request URL.
fn pr_number(url: Option<&String>) -> Option<u32> {
    url?.rsplit('/').next()?.parse().ok()
}

/// Parse `type(scope)!: subject`, or nothing if the head is not a type.
fn split_conventional(text: &str) -> Option<(String, Vec<String>, bool, String)> {
    let (head, subject) = text.split_once(':')?;
    let head = head.trim();
    let (head, breaking) = match head.strip_suffix('!') {
        Some(rest) => (rest, true),
        None => (head, false),
    };

    let (kind, scopes) = match head.split_once('(') {
        Some((kind, rest)) => {
            let inner = rest.strip_suffix(')')?;
            let scopes = inner
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            (kind.trim(), scopes)
        }
        None => (head, Vec::new()),
    };

    // A prose line can contain a colon too, so insist the head looks like a type.
    if kind.is_empty()
        || kind.len() > 12
        || !kind.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
    {
        return None;
    }
    Some((
        kind.to_ascii_lowercase(),
        scopes,
        breaking,
        subject.trim().to_owned(),
    ))
}

/// Parse one bullet.
fn parse_bullet(text: &str) -> Change {
    let (head, author, url) = split_attribution(text);
    let pr = pr_number(url.as_ref());
    match split_conventional(head) {
        Some((kind, scopes, breaking, subject)) => Change {
            kind,
            scopes,
            subject,
            breaking,
            author,
            pr,
            url,
        },
        None => Change {
            kind: String::new(),
            scopes: Vec::new(),
            subject: head.to_owned(),
            breaking: false,
            author,
            pr,
            url,
        },
    }
}

/// Parse a whole release body.
///
/// Duplicate entries are collapsed: some releases carry the generated block
/// twice, so the same pull request would otherwise be listed repeatedly.
#[must_use]
pub fn parse(body: &str) -> Notes {
    let mut notes = Notes::default();
    let mut seen: Vec<(Option<u32>, String)> = Vec::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("**Full Changelog**")
            || line.starts_with("Full Changelog:")
        {
            continue;
        }

        let bullet = line
            .strip_prefix("* ")
            .or_else(|| line.strip_prefix("- "))
            .or_else(|| line.strip_prefix("*\t"));
        let Some(bullet) = bullet else {
            if !notes.prose.iter().any(|seen| seen == line) {
                notes.prose.push(line.to_owned());
            }
            continue;
        };

        let change = parse_bullet(bullet);
        let key = (change.pr, change.subject.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        if change.ships() {
            notes.shipped.push(change);
        } else {
            notes.other.push(change);
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real apps-v1.3.0 body, abridged but not reshaped.
    const BODY: &str = "\
## What's Changed
* feat(treadmill): per-record distance stream so Strava draws the pace graph by @rryles in https://github.com/UNAWatch/una-sdk/pull/185
* fix(docs): multi-version Pages deploy that preserves prior versions by @rryles in https://github.com/UNAWatch/una-sdk/pull/190
* fix(apps): show accumulated ascent for summary \"ELEV. GAIN\" by @rryles in https://github.com/UNAWatch/una-sdk/pull/195
* build(sims): make TouchGFXEnvPath overridable via environment variable by @rryles in https://github.com/UNAWatch/una-sdk/pull/198
* fix(tools): set the execute bit on the Linux converter binaries by @tobymurray in https://github.com/UNAWatch/una-sdk/pull/186
* chore: store all text files as LF by @tobymurray in https://github.com/UNAWatch/una-sdk/pull/188

**Full Changelog**: https://github.com/UNAWatch/una-sdk/compare/apps-v1.2.0...apps-v1.3.0";

    #[test]
    fn app_changes_are_separated_from_repository_changes() {
        let notes = parse(BODY);
        let shipped: Vec<_> = notes.shipped.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(
            shipped,
            [
                "per-record distance stream so Strava draws the pace graph",
                "show accumulated ascent for summary \"ELEV. GAIN\"",
            ]
        );
        // docs, sims, tools and a bare chore all belong to the repository.
        assert_eq!(notes.other.len(), 4);
        // Headings and the changelog footer are not content.
        assert!(notes.prose.is_empty());
    }

    #[test]
    fn a_bullet_keeps_its_author_and_pull_request() {
        let change = &parse(BODY).shipped[0];
        assert_eq!(change.kind, "feat");
        assert_eq!(change.scopes, ["treadmill"]);
        assert_eq!(change.author.as_deref(), Some("rryles"));
        assert_eq!(change.pr, Some(185));
        assert_eq!(
            change.url.as_deref(),
            Some("https://github.com/UNAWatch/una-sdk/pull/185")
        );
    }

    #[test]
    fn a_repeated_block_is_listed_once() {
        // apps-v1.2.0 really does carry its generated notes twice.
        let doubled = format!("{BODY}\n{BODY}");
        assert_eq!(parse(&doubled).shipped.len(), parse(BODY).shipped.len());
        assert_eq!(parse(&doubled).other.len(), parse(BODY).other.len());
    }

    #[test]
    fn simulator_work_scoped_to_an_app_is_still_simulator_work() {
        // All four are real apps-v1.3.0 lines whose scope names an app.
        for line in [
            "* fix(hrmonitor): make the GCC/Linux simulator build",
            "* fix(files): link FitWriter into the simulator",
            "* fix(treadmill): link the calibration sources into the simulator",
            "* fix(workout): correct sim TouchGFXEnvPath so textconvert finds Ruby",
        ] {
            let notes = parse(line);
            assert!(notes.shipped.is_empty(), "should not ship: {line}");
            assert_eq!(notes.other.len(), 1);
        }
    }

    #[test]
    fn only_whole_words_name_the_simulator() {
        // "similar" and "simulate" are not the simulator.
        let notes = parse(
            "* fix(alarm): behave similarly to the clock\n             * feat(workout): simulate a paused lap correctly",
        );
        assert_eq!(notes.shipped.len(), 2, "{:?}", notes.other);
    }

    #[test]
    fn every_scope_must_be_off_device_to_demote_a_change() {
        // A fix touching both an app and the tooling still reaches the watch.
        let mixed = parse("* fix(treadmill,tools): both at once");
        assert_eq!(mixed.shipped.len(), 1);
        assert!(mixed.other.is_empty());

        let multi = parse("* fix(treadmill,running): intervals lap button advances the phase");
        assert_eq!(multi.shipped[0].scopes, ["treadmill", "running"]);
    }

    #[test]
    fn an_unrecognised_line_is_kept_rather_than_dropped() {
        // Not a conventional commit, so nothing is known about it: it ships.
        let notes = parse("* Rewrote the thing\nSome free prose.\n");
        assert_eq!(notes.shipped.len(), 1);
        assert_eq!(notes.shipped[0].kind, "");
        assert_eq!(notes.shipped[0].subject, "Rewrote the thing");
        assert_eq!(notes.prose, ["Some free prose."]);
    }

    #[test]
    fn a_colon_in_prose_is_not_mistaken_for_a_type() {
        let notes = parse("* Fixed the thing, finally: it works now by @someone");
        assert_eq!(notes.shipped[0].kind, "");
        assert_eq!(
            notes.shipped[0].subject,
            "Fixed the thing, finally: it works now"
        );
        assert_eq!(notes.shipped[0].author.as_deref(), Some("someone"));
    }

    #[test]
    fn a_breaking_change_is_flagged() {
        let notes = parse("* feat(sensor)!: getXYZ returns floats");
        assert!(notes.shipped[0].breaking);
        assert!(notes.shipped[0].summary().starts_with("breaking: "));

        let bare = parse("* feat!: everything is different");
        assert!(bare.shipped[0].breaking);
        assert_eq!(bare.shipped[0].kind, "feat");
    }

    #[test]
    fn an_empty_body_yields_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("## What's Changed\n\n").is_empty());
    }

    #[test]
    fn a_summary_reads_without_the_type_prefix() {
        let notes = parse("* fix(workout): stop HR-zone summary time clipping");
        assert_eq!(
            notes.shipped[0].summary(),
            "workout — stop HR-zone summary time clipping"
        );
    }
}
