//! `kira` — build the catalogue, serve it locally, and generate the site icons.

mod build;
mod icons;
mod serve;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Tooling for Kira, an app store for UNA Watch.
#[derive(Debug, Parser)]
#[command(name = "kira", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build the catalogue from one or more unzipped una-apps releases.
    ///
    /// Input is the release zips' own layout — one directory per app, each
    /// holding exactly one .uapp — either directly under --src for a single
    /// release, or nested one level under a release tag:
    ///
    ///   <src>/apps-v1.3.0/GlanceHR/Live_HR_1.3.0.uapp   (multi-release)
    ///   <src>/`GlanceHR/Live_HR_1.3.0.uapp`               (single, tag from --tag)
    ///
    /// Does no network I/O: release notes and dates come from --releases, which
    /// the workflow fetches, so builds stay hermetic.
    Build {
        /// Directory holding the unzipped release(s).
        #[arg(long)]
        src: PathBuf,
        /// Site directory to write `data/` into.
        #[arg(long, default_value = "site")]
        out: PathBuf,
        /// JSON array of {tag, publishedAt, url, isPrerelease, notes}.
        #[arg(long)]
        releases: Option<PathBuf>,
        /// Upstream repository, recorded in the catalogue.
        #[arg(long)]
        repo: Option<String>,
        /// Tag to assume when --src holds a single release.
        #[arg(long)]
        tag: Option<String>,
    },

    /// Serve the site locally over HTTP.
    ///
    /// The File System Access API needs a secure context, and <http://localhost>
    /// counts as one — opening site/index.html over file:// does not work.
    Serve {
        /// Directory to serve.
        #[arg(long, default_value = "site")]
        root: PathBuf,
        /// Port to listen on.
        #[arg(long, default_value_t = 8099)]
        port: u16,
    },

    /// Regenerate the favicons and link-preview card from the source artwork.
    Icons {
        /// Source artwork.
        #[arg(long, default_value = "assets/kira-mark.png")]
        src: PathBuf,
        /// Site directory to write `img/` and `favicon.ico` into.
        #[arg(long, default_value = "site")]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Build {
            src,
            out,
            releases,
            repo,
            tag,
        } => build::run(&build::Args {
            src,
            out,
            releases,
            repo,
            tag,
        }),
        Command::Serve { root, port } => serve::run(&root, port),
        Command::Icons { src, out } => icons::run(&src, &out),
    }
}
