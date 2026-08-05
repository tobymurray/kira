//! Where a build sits among others stamped with the same version number.
//!
//! Upstream publishes `apps-v1.4.0-rc1` weeks before `apps-v1.4.0`, and an app
//! that ships for the first time in a release candidate is otherwise unreachable
//! until the final lands. Kira publishes those candidates so nobody has to build
//! from source or wait — but only where it can still say, truthfully, which build
//! is the newer one.
//!
//! That is harder than it sounds, and the reason is in the binary. The packer
//! stamps the version parsed from the tag, so **`Stopwatch_1.4.0-rc1.uapp` reports
//! itself as 1.4.0, byte for byte identically to how the final release will**.
//! Nothing in a `.uapp` header distinguishes a candidate from the release it
//! became. [`crate::uapp::Version`] is that header field and nothing else, so it
//! is deliberately left alone: it is the identity read off a watch, and widening
//! it to carry a stage would make it disagree with the device.
//!
//! So precedence lives here instead, keyed off the tag, which is the only place
//! the distinction exists. The rule is semver's: a pre-release ranks below the
//! release of the same version, and above everything below it.
//!
//! ```text
//! 1.3.0  <  1.4.0-rc1  <  1.4.0-rc2  <  1.4.0
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::uapp::Version;

/// A build's rank among those sharing its [`Version`].
///
/// Ordered, and the ordering is the point: [`Stage::Final`] is greater than every
/// [`Stage::Candidate`], so `derive(Ord)` on this and on the field order of
/// [`Precedence`] is what makes `1.4.0-rc1 < 1.4.0` fall out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// A release candidate, with its number. `rc1` is 1.
    ///
    /// An unnumbered or unparseable suffix is 0, which ranks below every numbered
    /// candidate — the conservative direction, since the alternative is claiming a
    /// build is newer than one that might supersede it.
    Candidate(u32),
    /// A full release. Outranks every candidate of the same version.
    Final,
}

/// A build's full ordering key: version first, then stage.
///
/// Field order is the comparison order, so 1.3.0 final still sits below any
/// 1.4.0 candidate. That is the half of semver precedence people forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Precedence {
    /// The version the binary stamps.
    pub version: Version,
    /// Where this build sits among builds stamping that same version.
    pub stage: Stage,
}

/// The pre-release part of a release tag, when it has one.
///
/// Stored rather than re-derived because the catalogue is the record of what was
/// published, and the tag it came from is already beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PreRelease(String);

impl PreRelease {
    /// Read the suffix out of a tag, e.g. `rc1` from `apps-v1.4.0-rc1`.
    ///
    /// `None` for a tag naming a full release. Keyed on the *last* `-`, since the
    /// prefix convention (`apps-v…`) contains one of its own.
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        let (_, suffix) = tag.rsplit_once('-')?;
        // `apps-v1.4.0` splits to `v1.4.0`, which is a version and not a stage.
        // Anything starting with a digit or a `v` is part of the version.
        let first = suffix.chars().next()?;
        if first.is_ascii_digit() || matches!(first, 'v' | 'V') {
            return None;
        }
        Some(Self(suffix.to_owned()))
    }

    /// The label as written in the tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Rank among candidates of the same version.
    #[must_use]
    pub fn stage(&self) -> Stage {
        let digits: String = self.0.chars().filter(char::is_ascii_digit).collect();
        Stage::Candidate(digits.parse().unwrap_or(0))
    }
}

impl fmt::Display for PreRelease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The ordering key for a version and its optional pre-release label.
#[must_use]
pub fn precedence(version: Version, prerelease: Option<&PreRelease>) -> Precedence {
    Precedence {
        version,
        stage: prerelease.map_or(Stage::Final, PreRelease::stage),
    }
}

/// How a build is named to a reader, and how a selection refers to it.
///
/// `1.4.0` or `1.4.0-rc1`. Unique per published build of an app, which the bare
/// version is not once candidates are published — two entries would both be
/// "1.4.0" and a picker could not tell them apart.
#[must_use]
pub fn label(version: Version, prerelease: Option<&PreRelease>) -> String {
    match prerelease {
        Some(pre) => format!("{version}-{pre}"),
        None => version.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn a_candidate_is_read_out_of_its_tag() {
        assert_eq!(
            PreRelease::from_tag("apps-v1.4.0-rc1").map(|p| p.as_str().to_owned()),
            Some("rc1".to_owned())
        );
        assert_eq!(
            PreRelease::from_tag("apps-v0.1.9-rc3").map(|p| p.as_str().to_owned()),
            Some("rc3".to_owned())
        );
        // A full release has no stage, and the `-` in the prefix is not one.
        assert_eq!(PreRelease::from_tag("apps-v1.3.0"), None);
        assert_eq!(PreRelease::from_tag("v1.3.0"), None);
        assert_eq!(PreRelease::from_tag("1.3.0"), None);
    }

    #[test]
    fn a_candidate_ranks_below_the_release_it_becomes() {
        // The whole point, and the thing Version alone cannot express: both of
        // these binaries stamp themselves 1.4.0.
        let rc1 = precedence(v("1.4.0"), PreRelease::from_tag("apps-v1.4.0-rc1").as_ref());
        let rc2 = precedence(v("1.4.0"), PreRelease::from_tag("apps-v1.4.0-rc2").as_ref());
        let final_ = precedence(v("1.4.0"), None);
        assert!(rc1 < rc2, "rc1 should precede rc2");
        assert!(rc2 < final_, "a candidate should precede its release");
    }

    #[test]
    fn a_candidate_still_outranks_every_earlier_release() {
        // The half of semver precedence that is easy to get backwards.
        let previous = precedence(v("1.3.0"), None);
        let rc1 = precedence(v("1.4.0"), PreRelease::from_tag("apps-v1.4.0-rc1").as_ref());
        assert!(previous < rc1, "1.3.0 should precede 1.4.0-rc1");
    }

    #[test]
    fn an_unnumbered_stage_ranks_below_a_numbered_one() {
        // Refusing to guess: claiming otherwise would offer a build as newer than
        // one that may supersede it.
        let beta = precedence(
            v("1.4.0"),
            PreRelease::from_tag("apps-v1.4.0-beta").as_ref(),
        );
        let rc1 = precedence(v("1.4.0"), PreRelease::from_tag("apps-v1.4.0-rc1").as_ref());
        assert!(beta < rc1);
    }

    #[test]
    fn a_label_distinguishes_builds_a_version_cannot() {
        let pre = PreRelease::from_tag("apps-v1.4.0-rc1");
        assert_eq!(label(v("1.4.0"), pre.as_ref()), "1.4.0-rc1");
        assert_eq!(label(v("1.4.0"), None), "1.4.0");
        assert_ne!(label(v("1.4.0"), pre.as_ref()), label(v("1.4.0"), None));
    }

    #[test]
    fn ordering_sorts_a_whole_history_the_way_semver_does() {
        let mut builds = [
            (v("1.4.0"), None),
            (v("1.3.0"), None),
            (v("1.4.0"), PreRelease::from_tag("apps-v1.4.0-rc2")),
            (v("1.4.0"), PreRelease::from_tag("apps-v1.4.0-rc1")),
        ];
        builds.sort_by_key(|(ver, pre)| precedence(*ver, pre.as_ref()));
        let order: Vec<String> = builds
            .iter()
            .map(|(ver, pre)| label(*ver, pre.as_ref()))
            .collect();
        assert_eq!(order, ["1.3.0", "1.4.0-rc1", "1.4.0-rc2", "1.4.0"]);
    }
}
