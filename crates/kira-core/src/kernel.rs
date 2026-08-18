//! Which kernel a build needs, and which builds a kernel will start.
//!
//! An app refuses to start on a kernel older than the `KERNEL_INTERFACE_VERSION`
//! it was compiled against. It stops before drawing anything, so what the owner
//! sees is an app that will not open and cannot be exited — which reads as a
//! broken watch rather than a wrong choice.
//!
//! Nothing in a `.uapp` says which kernel it needs. The requirement is compiled
//! in, `minKernelVersion` is a BLE/DIS check in the official mobile app, and the
//! watch reports its firmware only over Bluetooth, which the page has no access
//! to. See `UNAWatch/una-sdk#262`. So this cannot be byte-derived and cannot be
//! detected — but it can be *worked out*, because what a build was compiled
//! against is recorded for every build in the catalogue: upstream's own binaries
//! carry the release tag they shipped in, and everything Kira compiles carries
//! `builtFrom.sdkRev`. Given which release stamped which interface version, the
//! requirement follows.
//!
//! That mapping is the one assertion here that no byte supports, so it is kept
//! small, ordered and sourced — and it refuses to answer for a release newer than
//! the one it was last checked against. [`CHECKED_THROUGH`] is the whole safety
//! story: assuming a later release *did not* bump the interface is the dangerous
//! direction, because it would claim a build runs on a kernel that will refuse
//! it. Saying "cannot say" leaves the page hedging, which is where it started.

use serde::{Deserialize, Serialize};

use crate::catalog::{App, Catalog, Release, VersionEntry, version_from_tag};
use crate::prerelease::{self, PreRelease, Precedence};

/// A kernel ABI generation: the watch's `KERNEL_INTERFACE_VERSION`.
pub type Interface = u8;

/// What every SDK release stamped before the first bump in [`BUMPS`].
const BASE: Interface = 2;

/// Every `KERNEL_INTERFACE_VERSION` bump, oldest first, keyed by the SDK release
/// that first carried it.
///
/// 2 → 3 landed in `UNAWatch/una-sdk#236`, for the app-pushed home-screen widget
/// IPC channel, and first shipped in `apps-v1.4.0-rc1` — which is the earliest
/// release whose notes name that pull request.
///
/// Adding an entry here is the whole of teaching Kira about a new kernel: the
/// default the catalogue presents as, the choices offered, and what each build
/// requires all follow from this list. Move [`CHECKED_THROUGH`] in the same edit.
const BUMPS: &[(&str, Interface)] = &[("apps-v1.4.0-rc1", 3)];

/// The newest SDK release this table has been checked against.
///
/// A build from anything newer gets no answer rather than a guess. Without this,
/// the day upstream bumps the interface again every 1.5 build would be reported
/// as needing what 1.4 needed, and offered to a 1.4 watch that cannot start it.
/// Bump this when a release has been read and found not to move the interface,
/// which is a smaller claim than the table's other entries and the reason it is
/// separate from them.
const CHECKED_THROUGH: &str = "apps-v1.4.0";

/// A firmware generation the viewer can say their watch is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Firmware {
    /// How the choice reads on the page.
    pub label: String,
    /// The interface version that firmware provides.
    pub interface: Interface,
    /// Whether the catalogue presents as this until told otherwise.
    pub is_default: bool,
}

/// Where a release tag sits in the published order.
///
/// The stage comes from upstream's own pre-release flag as the catalogue recorded
/// it, never from the tag's suffix: `apps-v0.1.9-rc1`, `-rc2` and `-rc3` are full
/// releases whose tags read like candidates, and [`crate::prerelease`] documents
/// what trusting the suffix cost. A tag the catalogue does not list is ranked as a
/// full release, which is what the table's own constants are.
fn rank(releases: &[Release], tag: &str) -> Option<Precedence> {
    let version = version_from_tag(tag)?;
    let marked = releases
        .iter()
        .find(|release| release.tag == tag)
        .is_some_and(|release| release.is_prerelease);
    Some(prerelease::precedence(
        version,
        PreRelease::for_release(tag, marked).as_ref(),
    ))
}

/// The interface version an SDK release stamps into everything built from it.
///
/// `None` where the table cannot say: an unparseable tag, or a release newer than
/// [`CHECKED_THROUGH`].
#[must_use]
pub fn interface_of(releases: &[Release], sdk_rev: &str) -> Option<Interface> {
    let built = rank(releases, sdk_rev)?;
    if built > rank(releases, CHECKED_THROUGH)? {
        return None;
    }
    let mut interface = BASE;
    for (tag, bumped) in BUMPS {
        if rank(releases, tag).is_some_and(|at| built >= at) {
            interface = *bumped;
        }
    }
    Some(interface)
}

/// The interface version the catalogue presents as until told otherwise.
#[must_use]
pub fn newest() -> Interface {
    BUMPS.last().map_or(BASE, |&(_, interface)| interface)
}

/// The choices to offer, newest first.
///
/// Newest is the default, and there is deliberately no "not sure": it would
/// reinstate the hedging on every card that presenting as current firmware exists
/// to remove, and somebody who does not know is better served by being told where
/// to read the version off the watch than by a third state that makes every card
/// vaguer.
#[must_use]
pub fn firmwares() -> Vec<Firmware> {
    let short = |tag: &str| {
        version_from_tag(tag).map(|version| format!("{}.{}", version.major(), version.minor()))
    };
    let default = newest();
    let mut out = Vec::with_capacity(BUMPS.len() + 1);

    // Newest first, so each bump is described by the release that introduced it
    // and bounded by the one above it where there is one.
    for (index, (tag, interface)) in BUMPS.iter().enumerate().rev() {
        let floor = short(tag).unwrap_or_else(|| (*tag).to_owned());
        let label = match BUMPS.get(index + 1).and_then(|&(above, _)| short(above)) {
            Some(ceiling) => format!("{floor} up to {ceiling}"),
            None => format!("{floor} or newer"),
        };
        out.push(Firmware {
            label,
            interface: *interface,
            is_default: *interface == default,
        });
    }

    // Everything below the first bump. Named by what it is older than, because
    // the table knows where the boundary is and not how far back support goes.
    let base_label = BUMPS.first().and_then(|&(tag, _)| short(tag)).map_or_else(
        || "any firmware".to_owned(),
        |first| format!("older than {first}"),
    );
    out.push(Firmware {
        label: base_label,
        interface: BASE,
        is_default: BASE == default,
    });
    out
}

impl Catalog {
    /// The interface version this build needs, or `None` where it cannot be said.
    ///
    /// The SDK revision is the recipe's where Kira compiled the binary — which is
    /// every submission, whose own `tag` is its manifest slug and says nothing
    /// about an SDK — and the release tag otherwise.
    #[must_use]
    pub fn interface_required(&self, entry: &VersionEntry) -> Option<Interface> {
        let sdk_rev = entry
            .built_from
            .as_ref()
            .map_or(entry.tag.as_str(), |built| built.sdk_rev.as_str());
        interface_of(&self.releases, sdk_rev)
    }

    /// Whether a watch on `firmware` will start this build.
    ///
    /// A build the table cannot place counts as runnable. The alternative is
    /// hiding something on a suspicion, and an unplaceable build is exactly the
    /// case the page still hedges about on the card.
    ///
    /// A variant alias is judged the same way as a binary. It carries no code, so
    /// the interface check cannot refuse it — but aliases arrived with the release
    /// that bumped the interface, and an alias an older kernel does not act on
    /// does nothing either way, so the answer this gives is the useful one.
    #[must_use]
    pub fn runs_on(&self, entry: &VersionEntry, firmware: Interface) -> bool {
        self.interface_required(entry)
            .is_none_or(|needs| needs <= firmware)
    }

    /// The build to offer for an app, given the firmware the view presents as.
    ///
    /// [`App::latest`]'s rule, narrowed to what will start: the newest full
    /// release that runs, then a candidate if that is the only thing that runs —
    /// the same generalisation `latest` already makes for an app with no full
    /// release at all — and finally `latest` itself, unchanged.
    ///
    /// That last fallback is deliberate. When nothing in an app will start on this
    /// firmware, the card still shows a build and says what it needs; an app that
    /// silently emptied itself would teach the owner nothing about why.
    #[must_use]
    pub fn default_build<'a>(&self, app: &'a App, firmware: Interface) -> &'a VersionEntry {
        app.versions
            .iter()
            .find(|entry| !entry.is_prerelease() && self.runs_on(entry, firmware))
            .or_else(|| {
                app.versions
                    .iter()
                    .find(|entry| self.runs_on(entry, firmware))
            })
            .unwrap_or_else(|| app.latest())
    }

    /// How many apps this firmware moves off the build the catalogue calls latest.
    ///
    /// Shown on the control that set it: a filtered catalogue that does not say how
    /// much it is filtering reads as a broken one.
    #[must_use]
    pub fn narrowed_by(&self, firmware: Interface) -> usize {
        self.apps
            .iter()
            .filter(|app| {
                self.default_build(app, firmware).precedence() != app.latest().precedence()
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{AppType, Origin, Source};
    use crate::uapp::{AppId, Version};

    /// A build of `version`, compiled against the SDK release named by `sdk_rev`.
    fn build(version: &str, sdk_rev: &str) -> VersionEntry {
        let parsed: Version = version.parse().unwrap();
        VersionEntry {
            version: parsed,
            version_packed: parsed.packed(),
            prerelease: PreRelease::for_release(sdk_rev, sdk_rev.contains("-rc")),
            supersedes_sha256: Vec::new(),
            tag: sdk_rev.to_owned(),
            folder: "Alarm".into(),
            file: format!("Alarm_{version}.uapp"),
            libc_version: Version::new(0, 0, 3),
            autostart: false,
            variant: None,
            size: 1024,
            sha256: format!("sha-{version}"),
            payload_sha256: format!("payload-{version}"),
            download: format!("apps/{sdk_rev}/Alarm/Alarm_{version}.uapp"),
            changed: Some(true),
            delta_bytes: Some(0),
            origin: Origin::Upstream,
            built_from: None,
            upstream_sha256: None,
            matches_upstream: None,
            retired: None,
            notes: None,
        }
    }

    fn app(versions: Vec<VersionEntry>) -> App {
        App {
            app_id: AppId::new(0xA19C_2A7E_4F8B_6D31),
            name: "Alarm".into(),
            app_type: AppType::Utility,
            folder: "Alarm".into(),
            versions,
            icon: None,
            icon_small: None,
            superseded_by: None,
            publisher: None,
            config: None,
            retired: None,
        }
    }

    fn release(tag: &str, is_prerelease: bool) -> Release {
        Release {
            tag: tag.to_owned(),
            published_at: None,
            url: None,
            is_prerelease,
            notes: None,
            app_count: 1,
        }
    }

    /// Upstream's real shape: candidates marked as such, and the 0.1.9 tags that
    /// read like candidates but are full releases.
    fn catalog(apps: Vec<App>) -> Catalog {
        Catalog {
            schema: crate::catalog::SCHEMA,
            generated: "2026-08-18T00:00:00Z".into(),
            source: Source { repo: None },
            releases: vec![
                release("apps-v1.4.0", false),
                release("apps-v1.4.0-rc1", true),
                release("apps-v1.3.0", false),
                release("apps-v0.1.9-rc1", false),
            ],
            apps,
        }
    }

    #[test]
    fn the_bump_is_placed_at_the_candidate_that_first_carried_it() {
        let c = catalog(Vec::new());
        assert_eq!(interface_of(&c.releases, "apps-v1.3.0"), Some(2));
        assert_eq!(interface_of(&c.releases, "apps-v1.4.0-rc1"), Some(3));
        assert_eq!(interface_of(&c.releases, "apps-v1.4.0"), Some(3));
    }

    #[test]
    fn a_tag_that_only_reads_like_a_candidate_is_ranked_by_upstreams_flag() {
        // apps-v0.1.9-rc1 is a full release. It sits far below the boundary either
        // way, so what this guards is the ranking, not the answer: reading the
        // suffix would make it a candidate of 0.1.9 rather than the release.
        let c = catalog(Vec::new());
        assert_eq!(interface_of(&c.releases, "apps-v0.1.9-rc1"), Some(2));
        assert_eq!(
            rank(&c.releases, "apps-v0.1.9-rc1"),
            rank(&[], "apps-v0.1.9")
        );
    }

    #[test]
    fn a_release_newer_than_the_table_gets_no_answer_rather_than_a_guess() {
        let c = catalog(Vec::new());
        assert_eq!(interface_of(&c.releases, "apps-v1.5.0"), None);
        assert_eq!(interface_of(&c.releases, "apps-v2.0.0-rc1"), None);
        // Not a release tag at all.
        assert_eq!(interface_of(&c.releases, "chrono"), None);
    }

    #[test]
    fn a_submission_is_placed_by_the_sdk_it_was_built_against() {
        // Its own tag is a manifest slug and says nothing about an SDK.
        let mut entry = build("0.1.0", "chrono");
        entry.origin = Origin::Kira;
        entry.built_from = Some(crate::catalog::BuiltFrom {
            app_source: "git:https://example.invalid/watch-apps@abc:Chrono".into(),
            sdk_rev: "apps-v1.3.0".into(),
            toolchain: "sha256:00".into(),
            recipe: "0000000000000000".into(),
        });
        let c = catalog(Vec::new());
        assert_eq!(c.interface_required(&entry), Some(2));
        assert!(c.runs_on(&entry, 2), "1.3-built runs on the older kernel");
        assert!(c.runs_on(&entry, 3), "and on the newer one");
    }

    #[test]
    fn the_default_firmware_offers_exactly_what_latest_offers() {
        // The no-regression guard, and the property the default view rests on.
        let apps = vec![
            app(vec![
                build("1.4.0", "apps-v1.4.0"),
                build("1.3.0", "apps-v1.3.0"),
            ]),
            app(vec![build("1.4.0", "apps-v1.4.0")]),
            app(vec![build("1.3.0", "apps-v1.3.0")]),
        ];
        let c = catalog(apps);
        for a in &c.apps {
            assert_eq!(
                c.default_build(a, newest()).label(),
                a.latest().label(),
                "presenting as current firmware must change nothing"
            );
        }
        assert_eq!(c.narrowed_by(newest()), 0);
    }

    #[test]
    fn an_older_kernel_is_offered_the_newest_build_that_will_start() {
        let c = catalog(vec![app(vec![
            build("1.4.0", "apps-v1.4.0"),
            build("1.3.0", "apps-v1.3.0"),
            build("1.2.0", "apps-v1.2.0"),
        ])]);
        let alarm = &c.apps[0];
        assert_eq!(c.default_build(alarm, 2).label(), "1.3.0");
        assert_eq!(c.default_build(alarm, 3).label(), "1.4.0");
        assert_eq!(c.narrowed_by(2), 1);
    }

    #[test]
    fn an_app_with_nothing_runnable_still_shows_a_build() {
        // Stopwatch's case: it ships in 1.4 and there is no older build of it. An
        // app that emptied itself would say nothing about why.
        let c = catalog(vec![app(vec![build("1.4.0", "apps-v1.4.0")])]);
        let stopwatch = &c.apps[0];
        assert_eq!(c.default_build(stopwatch, 2).label(), "1.4.0");
        assert!(!c.runs_on(c.default_build(stopwatch, 2), 2));
    }

    #[test]
    fn a_candidate_is_offered_only_when_it_is_the_only_thing_that_runs() {
        // latest()'s rule, narrowed: a full release outranks a candidate, but a
        // full release that cannot start is not an offer.
        let c = catalog(vec![app(vec![
            build("1.4.0", "apps-v1.4.0"),
            build("1.4.0", "apps-v1.4.0-rc1"),
            build("1.3.0", "apps-v1.3.0"),
        ])]);
        let a = &c.apps[0];
        assert_eq!(c.default_build(a, 3).label(), "1.4.0");
        assert_eq!(c.default_build(a, 2).label(), "1.3.0");

        // With no older full release, the runnable candidate is the offer.
        let c = catalog(vec![app(vec![
            build("1.4.0", "apps-v1.4.0"),
            build("1.3.0", "apps-v1.3.0-rc1"),
        ])]);
        assert_eq!(c.default_build(&c.apps[0], 2).label(), "1.3.0-rc1");
    }

    #[test]
    fn the_choices_are_newest_first_and_name_their_boundary() {
        let choices = firmwares();
        assert_eq!(choices.len(), BUMPS.len() + 1);
        assert_eq!(choices[0].interface, newest());
        assert!(
            choices[0].is_default,
            "the newest is what the page presents as"
        );
        assert_eq!(choices[0].label, "1.4 or newer");
        assert_eq!(choices[choices.len() - 1].label, "older than 1.4");
        assert_eq!(choices[choices.len() - 1].interface, BASE);
        assert!(choices[1..].iter().all(|choice| !choice.is_default));
    }
}
