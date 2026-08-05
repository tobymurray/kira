//! Core logic for Kira, an app store for UNA Watch.
//!
//! Split so that the same code serves the catalogue build and the browser:
//!
//! - [`uapp`] reads the `.uapp` container, borrowing rather than copying.
//! - [`icon`] decodes the ABGR2222 icons embedded in one.
//! - [`catalog`] is the published schema, plus version selection.
//! - [`config`] assembles the settings file an app reads from its own folder.
//! - [`fat`] names the watch's volume, or a desktop, will not take literally.
//! - [`notes`] sorts upstream's release prose by what can reach a watch.
//! - [`prerelease`] ranks builds that stamp the same version, e.g. `1.4.0-rc1`.
//! - [`plan`] diffs a selection against a watch and generates installers.
//!
//! Nothing here does I/O, so it compiles to WebAssembly unchanged.

pub mod catalog;
pub mod config;
pub mod fat;
pub mod icon;
pub mod notes;
pub mod plan;
pub mod prerelease;
pub mod uapp;
