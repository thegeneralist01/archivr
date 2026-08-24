use anyhow::{bail, Context, Result};
use archivr_core::{
    archive,
    capture::CaptureConfig,
    downloader::ytdlp::{
        forced_yt_dlp, pinned_yt_dlp, probe_version, resolve_yt_dlp, state_dir, state_dir_yt_dlp,
    },
};
use clap::{Parser, Subcommand};
use std::{
    env,
    path::{Path, PathBuf},
    process,
    process::Command as ProcCommand,
};

/// GitHub release metadata endpoint for the upstream yt-dlp project.
const YT_DLP_LATEST_RELEASE: &str =
    "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";

/// Every python zipapp starts with this shebang; used as a sanity check that we
/// downloaded the artifact and not an HTML error page or an LFS pointer.
const ZIPAPP_SHEBANG: &[u8] = b"#!/usr/bin/env python3";

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Archive the specified file or directory
    Archive {
        /// URL or Path to archive
        path: String,
    },
    Init {
        /// Path to initialize the archive in
        #[arg(default_value = ".")]
        path: String,

        /// Store path - path to store the archived files in.
        /// Structure will be:
        /// store_path/
        ///   temp/
        ///     ...
        ///   raw/
        ///     ...
        ///   raw_tweets/
        ///     ...
        ///   structured/
        ///     ...
        #[arg(default_value = "./.archivr/store")]
        store_path: String,

        /// Name of the archive
        #[arg(short, long)]
        name: String,

        /// Wipe existing .archivr repository data
        #[arg(long = "force-with-info-removal")]
        force_with_info_removal: bool,
    },

    /// Inspect or update the yt-dlp binary archivr runs
    #[command(name = "yt-dlp")]
    YtDlp {
        #[command(subcommand)]
        subcmd: YtDlpCmd,
    },
}

#[derive(Subcommand, Debug)]
enum YtDlpCmd {
    /// Download the latest yt-dlp zipapp into archivr's state directory
    Update {
        /// Install this exact release tag instead of the latest (e.g. 2026.09.15)
        #[arg(long)]
        version: Option<String>,
    },
    /// Show every yt-dlp candidate, its version, and which one wins
    Status,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Archive { ref path } => {
            let archive_path = match archive::find_archive_path()? {
                Some(path) => path,
                None => {
                    eprintln!("Not in an archive. Use 'archivr init' to create one.");
                    process::exit(1);
                }
            };
            let archive_paths = archive::read_archive_paths(&archive_path)?;
            let result = archivr_core::capture::perform_capture(&archive_paths, path, None, None, &CaptureConfig::default())?;
            println!("Archived: run {}", result.run_uid);
            Ok(())
        }

        Command::Init {
            path: ref archive_path_string,
            store_path: ref store_path_string,
            name: ref archive_name,
            force_with_info_removal,
        } => {
            let archive_parent = Path::new(&archive_path_string);
            let store_path = if Path::new(&store_path_string).is_relative() {
                env::current_dir()
                    .context("failed to read current working directory")?
                    .join(store_path_string)
            } else {
                Path::new(store_path_string).to_path_buf()
            };

            let paths = archive::initialize_archive(
                archive_parent,
                &store_path,
                archive_name,
                force_with_info_removal,
            )?;

            println!(
                "Initialized empty archive in {}",
                paths.archive_path.display()
            );

            Ok(())
        }

        Command::YtDlp { subcmd } => match subcmd {
            YtDlpCmd::Update { version } => yt_dlp_update(version.as_deref()),
            YtDlpCmd::Status => yt_dlp_status(),
        },
    }
}


/// Resolves `<state_dir>/yt-dlp/`, erroring out if there is no usable HOME.
fn yt_dlp_state_dir() -> Result<PathBuf> {
    state_dir()
        .map(|d| d.join("yt-dlp"))
        .context("could not determine a state directory (is $HOME set?)")
}

/// Formats one `status` row. Missing candidates show an em dash.
fn format_status_row(role: &str, path: Option<&Path>, chosen: &Path) -> String {
    match path {
        Some(p) => {
            let version = probe_version(p).unwrap_or_else(|| "—".to_string());
            let star = if p == chosen { "*" } else { "" };
            format!("{role}\t{}\t{version}\t{star}", p.display())
        }
        None => format!("{role}\t—\t—\t"),
    }
}

/// Prints one `status` row. Missing candidates show an em dash.
fn status_row(role: &str, path: Option<&Path>, chosen: &Path) {
    println!("{}", format_status_row(role, path, chosen));
}

fn yt_dlp_status() -> Result<()> {
    let chosen = resolve_yt_dlp();

    println!("role\tpath\tversion\tchosen");
    status_row(
        "force (ARCHIVR_YT_DLP_FORCE)",
        forced_yt_dlp().as_deref(),
        &chosen,
    );
    status_row("env (ARCHIVR_YT_DLP)", pinned_yt_dlp().as_deref(), &chosen);

    // Show the state-dir slot even when empty, so users can see where an
    // `archivr yt-dlp update` would land.
    let state_candidate = state_dir_yt_dlp().filter(|p| p.is_file());
    status_row("state-dir", state_candidate.as_deref(), &chosen);

    status_row(
        "path-fallback (yt-dlp)",
        Some(Path::new("yt-dlp")),
        &chosen,
    );

    if let Ok(dir) = yt_dlp_state_dir() {
        if state_dir_yt_dlp().is_none_or(|p| !p.is_file()) {
            println!("\nNo state-dir install yet; `archivr yt-dlp update` would write to {}", dir.join("yt-dlp").display());
        }
    }

    Ok(())
}

/// Asks the GitHub API for the newest yt-dlp release tag.
fn latest_yt_dlp_version(client: &reqwest::blocking::Client) -> Result<String> {
    let body = client
        .get(YT_DLP_LATEST_RELEASE)
        .send()
        .context("failed to reach the GitHub releases API")?
        .error_for_status()
        .context("GitHub releases API returned an error")?
        .text()
        .context("failed to read the GitHub releases API response")?;

    let json: serde_json::Value =
        serde_json::from_str(&body).context("GitHub releases API returned invalid JSON")?;

    json.get("tag_name")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .context("GitHub releases API response had no tag_name")
}

fn yt_dlp_update(requested_version: Option<&str>) -> Result<()> {
    let dir = yt_dlp_state_dir()?;
    let target = dir.join("yt-dlp");
    let staging = dir.join("yt-dlp.new");
    let version_file = dir.join(".version");

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("archivr-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build an HTTP client")?;

    let version = match requested_version {
        Some(v) => v.to_string(),
        None => latest_yt_dlp_version(&client)?,
    };

    // The sibling .version file is what lets us skip a ~3MB download on a
    // no-op update; the binary itself is a zipapp with no cheap version probe
    // that doesn't cost a python startup.
    let installed = std::fs::read_to_string(&version_file).ok();
    if target.is_file() && installed.as_deref().map(str::trim) == Some(version.as_str()) {
        println!("yt-dlp {version} is already installed at {}", target.display());
        return Ok(());
    }

    println!("Downloading yt-dlp {version}…");
    let url = format!("https://github.com/yt-dlp/yt-dlp/releases/download/{version}/yt-dlp");
    let bytes = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed — is {version} a real release tag?"))?
        .bytes()
        .context("failed to read the downloaded yt-dlp body")?;

    if !bytes.starts_with(ZIPAPP_SHEBANG) {
        bail!(
            "downloaded artifact from {url} is not a python zipapp \
             (expected it to start with `{}`) — refusing to install it",
            String::from_utf8_lossy(ZIPAPP_SHEBANG)
        );
    }

    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    std::fs::write(&staging, &bytes)
        .with_context(|| format!("failed to write {}", staging.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("failed to chmod +x {}", staging.display()))?;
    }

    // Atomic swap: a concurrently-running archivr sees either the whole old
    // binary or the whole new one, never a half-written file.
    std::fs::rename(&staging, &target)
        .with_context(|| format!("failed to install {}", target.display()))?;
    std::fs::write(&version_file, format!("{version}\n"))
        .with_context(|| format!("failed to record version in {}", version_file.display()))?;

    // The zipapp is python source, not a native binary — installing it on a
    // host without python3 is legal (the server may run under a nix wrapper
    // with its own PATH) but worth flagging loudly.
    let has_python = ProcCommand::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_python {
        eprintln!(
            "warning: python3 was not found on PATH — the yt-dlp zipapp just installed \
             at {} will not run until python3 is available",
            target.display()
        );
    }

    println!("Installed yt-dlp {version} to {}", target.display());
    println!("archivr will now prefer it whenever it is newer than the pinned binary (ARCHIVR_YT_DLP).");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::format_status_row;
    use archivr_core::downloader::ytdlp::{
        forced_yt_dlp, resolve_yt_dlp_uncached, YT_DLP_FORCE_ENV,
    };
    use std::path::Path;

    fn fake_yt_dlp(path: &Path, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\necho {version}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn forced_candidate_is_rendered_and_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let forced = tmp.path().join("forced/yt-dlp");
        fake_yt_dlp(&forced, "2020.01.01");
        unsafe { std::env::set_var(YT_DLP_FORCE_ENV, &forced) };

        let candidate = forced_yt_dlp();
        assert_eq!(candidate.as_deref(), Some(forced.as_path()));
        let chosen = resolve_yt_dlp_uncached();
        assert_eq!(chosen, forced);
        assert_eq!(
            format_status_row(
                "force (ARCHIVR_YT_DLP_FORCE)",
                candidate.as_deref(),
                &chosen,
            ),
            format!(
                "force (ARCHIVR_YT_DLP_FORCE)\t{}\t2020.01.01\t*",
                forced.display()
            )
        );

        unsafe { std::env::remove_var(YT_DLP_FORCE_ENV) };
    }
}
