//! `kira` — build the catalogue, serve it locally, and generate the site icons.

mod build;
mod build_app;
mod icons;
mod recipe;
mod registry;
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

    /// Print what a .uapp says about itself, as JSON.
    ///
    /// Useful on its own for a downloaded binary, and used by the build pipeline:
    /// the version stamped into a release's binaries is not always the one in its
    /// tag -- apps-v0.1.9-rc1 contains binaries stamped 0.1.4 -- and the version
    /// is part of the recipe, so it has to be read rather than assumed.
    Inspect {
        /// The .uapp to read.
        file: PathBuf,
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

    /// Check or plan third-party app submissions in `registry/`.
    ///
    /// A submission is a manifest naming a repository, a commit and the SDK
    /// revision to build against — never an uploaded binary. See
    /// registry/README.md.
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
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

#[derive(Debug, Subcommand)]
enum RegistryCommand {
    /// Check every manifest, and refuse anything already published.
    ///
    /// Exits non-zero with the whole list of problems, so a contributor gets one
    /// round trip rather than one problem at a time.
    Validate {
        /// Directory of manifests.
        #[arg(long, default_value = "registry")]
        dir: PathBuf,
        /// The same directory as it exists on the base branch, to check that
        /// nothing already published has been rewritten.
        #[arg(long)]
        base: Option<PathBuf>,
        /// A published catalog.json, so a submission cannot claim an identity
        /// the catalogue already uses.
        #[arg(long)]
        catalog: Option<PathBuf>,
    },

    /// Emit what the submissions need built, in the same shape as `cache-plan`.
    Plan {
        /// Directory of manifests.
        #[arg(long, default_value = "registry")]
        dir: PathBuf,
        /// Toolchain identity, normally a container digest.
        #[arg(long)]
        toolchain: String,
        /// File of asset names already cached, one per line.
        #[arg(long)]
        available: Option<PathBuf>,
        /// Where to write the plan. Defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// Read a file of asset names, one per line.
fn read_available(path: Option<&PathBuf>) -> Result<std::collections::BTreeSet<String>> {
    let Some(path) = path else {
        return Ok(std::collections::BTreeSet::new());
    };
    Ok(std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
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

/// Run a `registry` subcommand.
///
/// Split out of `main` so the dispatch there stays a list of one-liners.
fn run_registry(command: RegistryCommand) -> Result<()> {
    match command {
        RegistryCommand::Validate { dir, base, catalog } => {
            let manifests = registry::load_dir(&dir)?;
            let (ids, folders) = match &catalog {
                Some(path) => registry::taken_from_catalog(path)?,
                None => Default::default(),
            };
            let mut problems = registry::validate(&manifests, &ids, &folders);
            if let Some(base) = &base {
                problems.extend(registry::check_unchanged(
                    &registry::load_dir(base)?,
                    &manifests,
                ));
            }

            eprintln!(
                "{} submission(s) in {}{}",
                manifests.len(),
                dir.display(),
                if catalog.is_some() {
                    ", checked against the published catalogue"
                } else {
                    ""
                }
            );
            print!("{}", registry::report(&problems));
            if problems.is_empty() {
                Ok(())
            } else {
                // A non-zero exit is what fails the pull request check.
                std::process::exit(1);
            }
        }
        RegistryCommand::Plan {
            dir,
            toolchain,
            available,
            out,
        } => {
            let wanted = registry::wanted(&registry::load_dir(&dir)?, &toolchain);
            let planned = recipe::plan(&wanted, &read_available(available.as_ref())?);
            let items = recipe::plan_items(&planned);
            let to_build = items.iter().filter(|i| i.action == "build").count();
            eprintln!(
                "{} submitted build(s): {} to build, {} already cached",
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
    }
}

/// Print what a `.uapp` says about itself.
///
/// # Errors
/// If the file cannot be read or is not a parseable `.uapp`.
fn inspect(file: &std::path::Path) -> Result<()> {
    let bytes =
        std::fs::read(file).map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;
    let uapp = kira_core::uapp::Uapp::parse(&bytes)?;
    let header = uapp.header();
    let crc = uapp.verify_crc();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "appId": header.app_id.to_string(),
            "name": header.name,
            "version": header.version.to_string(),
            "libcVersion": header.libc_version.to_string(),
            "type": header.app_type().to_string(),
            "autostart": header.autostart(),
            "size": bytes.len(),
            "serviceLen": header.service_len,
            "guiLen": uapp.gui_len(),
            "crcValid": crc.is_valid(),
            "sha256": sha256_hex(&bytes),
        }))?
    );
    Ok(())
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
            let planned = recipe::plan(&wanted, &read_available(available.as_ref())?);
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
        Command::Registry { command } => run_registry(command),
        Command::Inspect { file } => inspect(&file),
        Command::Serve { root, port } => serve::run(&root, port),
        Command::Icons { src, out } => icons::run(&src, &out),
    }
}
