//! `kira` — build the catalogue, serve it locally, and generate the site icons.

mod build;
mod build_app;
mod icons;
mod recipe;
mod serve;

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256.
///
/// sha2 0.11 returns a plain byte array with no hex formatting of its own, and
/// one dependency for that is not worth it.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

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
        /// Directory of binaries Kira has already built, named by recipe.
        ///
        /// Versions with a build here are served from it; the rest fall back to
        /// upstream's binary, and the catalogue records which is which.
        #[arg(long)]
        built: Option<PathBuf>,
        /// Toolchain identity the store was built with. Required with --built.
        #[arg(long)]
        toolchain: Option<String>,
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

    /// Build one app from source and verify the result.
    ///
    /// Runs cmake and the build tool directly, so run this inside the pinned
    /// toolchain container. The binary is checked against what its own
    /// CMakeLists.txt declares -- its id, type and version -- and against its own
    /// CRC, so a build that disagrees with its source fails rather than shipping.
    ///
    /// Kira passes its own -fmacro-prefix-map, so builds do not depend on where
    /// the trees sit whether or not the SDK carries that fix yet.
    BuildApp {
        /// The app's source tree, containing Software/.
        #[arg(long)]
        app: PathBuf,
        /// An SDK checkout to build against.
        #[arg(long)]
        sdk: PathBuf,
        /// Version to stamp into the binary.
        #[arg(long)]
        version: String,
        /// Where to write the verified .uapp.
        #[arg(long)]
        out: PathBuf,
        /// `CMake` generator. Output is identical either way.
        #[arg(long, default_value = "Unix Makefiles")]
        generator: String,
        /// Parallel build jobs. Defaults to the available parallelism.
        #[arg(long)]
        jobs: Option<usize>,
        /// Toolchain identity for the recipe, normally a container digest.
        #[arg(long, default_value = "unpinned")]
        toolchain: String,
        /// Canonical identity of the app source, for the recipe.
        #[arg(long, default_value = "unpinned")]
        app_source: String,
        /// SDK revision, for the recipe.
        #[arg(long, default_value = "unpinned")]
        sdk_rev: String,
    },

    /// Work out which built artifacts are already cached and which are missing.
    ///
    /// Reads app ids from the sources in an SDK checkout, so it needs no build and
    /// no network. Pass the asset names already in the cache via --available, one
    /// per line, and pipe the emitted JSON into whatever does the fetching.
    CachePlan {
        /// An SDK checkout whose Examples/Apps are to be built.
        #[arg(long)]
        sdk: PathBuf,
        /// SDK revision, for the recipe.
        #[arg(long)]
        sdk_rev: String,
        /// Version that would be stamped into these builds.
        #[arg(long)]
        version: String,
        /// Toolchain identity, normally a container digest.
        #[arg(long)]
        toolchain: String,
        /// File of asset names already cached, one per line. Absent means empty.
        #[arg(long)]
        available: Option<PathBuf>,
        /// Where to write the plan. Defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
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

/// Say so when a recipe input is missing, rather than minting a key that
/// identifies nothing and caching an artifact under it.
fn warn_if_unpinned(toolchain: &str, app_source: &str, sdk_rev: &str) {
    let unpinned: Vec<&str> = [
        ("toolchain", toolchain),
        ("app-source", app_source),
        ("sdk-rev", sdk_rev),
    ]
    .into_iter()
    .filter(|(_, value)| *value == "unpinned")
    .map(|(name, _)| name)
    .collect();
    if !unpinned.is_empty() {
        eprintln!(
            "warning: {} not given, so the recipe does not identify this build; \
             do not cache the result",
            unpinned.join(", ")
        );
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Build {
            src,
            out,
            releases,
            repo,
            tag,
            built,
            toolchain,
        } => build::run(&build::Args {
            src,
            out,
            releases,
            repo,
            tag,
            built,
            toolchain,
        }),
        Command::BuildApp {
            app,
            sdk,
            version,
            out,
            generator,
            jobs,
            toolchain,
            app_source,
            sdk_rev,
        } => {
            warn_if_unpinned(&toolchain, &app_source, &sdk_rev);
            let jobs = jobs.unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
            });
            build_app::run_build(&build_app::Args {
                app,
                sdk,
                version: version
                    .parse()
                    .map_err(|e| anyhow::anyhow!("--version: {e}"))?,
                out,
                generator,
                jobs,
                toolchain,
                app_source,
                sdk_rev,
            })
            .map(|_| ())
        }
        Command::CachePlan {
            sdk,
            sdk_rev,
            version,
            toolchain,
            available,
            out,
        } => {
            let version = version
                .parse()
                .map_err(|e| anyhow::anyhow!("--version: {e}"))?;
            let wanted = recipe::wanted_from_sdk(&sdk, &sdk_rev, &toolchain, version)?;
            let available = match available {
                Some(path) => std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect(),
                None => std::collections::BTreeSet::new(),
            };
            let planned = recipe::plan(&wanted, &available);
            let items = recipe::plan_items(&planned);
            let to_build = items.iter().filter(|i| i.action == "build").count();
            eprintln!(
                "{} apps: {} to build, {} already cached",
                items.len(),
                to_build,
                items.len() - to_build
            );
            let json = serde_json::to_string_pretty(&items)?;
            match out {
                Some(path) => std::fs::write(path, format!("{json}\n"))?,
                None => println!("{json}"),
            }
            Ok(())
        }
        Command::Serve { root, port } => serve::run(&root, port),
        Command::Icons { src, out } => icons::run(&src, &out),
    }
}
