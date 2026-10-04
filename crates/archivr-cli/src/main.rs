use anyhow::{bail, Context, Result};
use archivr_core::{
    archive,
    capture::CaptureConfig,
    downloader::ytdlp_tools::{tools_status, update_tools, ToolCandidate},
};
use clap::{Parser, Subcommand};
use std::{env, path::Path, process};

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
        /// Skip downloading YouTube subtitles
        #[arg(long)]
        no_subtitles: bool,
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

    /// Inspect or update the yt-dlp binary and the JavaScript runtime (Deno) archivr runs
    #[command(name = "yt-dlp")]
    YtDlp {
        #[command(subcommand)]
        subcmd: YtDlpCmd,
    },
}

#[derive(Subcommand, Debug)]
enum YtDlpCmd {
    /// Download the latest yt-dlp zipapp and Deno into archivr's state directory
    Update {
        /// Install this exact yt-dlp release tag instead of the latest (e.g. 2026.09.15);
        /// applies to yt-dlp only — Deno always installs the latest release
        #[arg(long)]
        version: Option<String>,
    },
    /// Show every yt-dlp and JS runtime candidate, its version, and which one wins
    Status,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Archive { ref path, no_subtitles } => {
            let archive_path = match archive::find_archive_path()? {
                Some(path) => path,
                None => {
                    eprintln!("Not in an archive. Use 'archivr init' to create one.");
                    process::exit(1);
                }
            };
            let archive_paths = archive::read_archive_paths(&archive_path)?;
            let config = CaptureConfig {
                download_subtitles: !no_subtitles,
                ..CaptureConfig::default()
            };
            let result = archivr_core::capture::perform_capture(&archive_paths, path, None, None, &config)?;
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

/// Formats one `status` row: `role\tlocation\tversion\tchosen`. Missing candidates and
/// unknown versions show an em dash.
fn format_status_row(
    role: &str,
    location: Option<&str>,
    version: Option<&str>,
    chosen: bool,
) -> String {
    match location {
        Some(loc) => {
            let version = version.unwrap_or("—");
            let star = if chosen { "*" } else { "" };
            format!("{role}\t{loc}\t{version}\t{star}")
        }
        None => format!("{role}\t—\t—\t"),
    }
}

/// Renders one candidate row; an invalid override or a candidate whose `--version`
/// probe fails shows its reason in the version column.
fn candidate_row(c: &ToolCandidate) -> String {
    match &c.invalid {
        Some(reason) => format_status_row(
            c.label,
            c.path.as_deref(),
            Some(&format!("invalid: {reason}")),
            c.chosen,
        ),
        None => format_status_row(c.label, c.path.as_deref(), c.version.as_deref(), c.chosen),
    }
}

fn yt_dlp_status() -> Result<()> {
    let s = tools_status();

    println!("role\tpath\tversion\tchosen");
    for c in &s.yt_dlp {
        println!("{}", candidate_row(c));
    }
    if let Some(target) = s.yt_dlp_target.as_deref().filter(|_| !s.yt_dlp_installed) {
        println!("\nNo state-dir install yet; `archivr yt-dlp update` would write to {target}");
    }

    println!("\nJS runtime (passed to yt-dlp as --js-runtimes)");
    println!("role\tpath\tversion\tchosen");
    for c in &s.js_runtime {
        println!("{}", candidate_row(c));
    }
    if s.js_runtime_chosen.is_none() {
        println!(
            "\nNo JS runtime resolved — YouTube downloads may fail with HTTP 403; run `archivr yt-dlp update`"
        );
    }
    if let Some(slot) = s.deno_target.as_deref().filter(|_| !s.deno_installed) {
        println!("\nNo state-dir deno yet; `archivr yt-dlp update` would write to {slot}");
    }
    Ok(())
}

/// Installs yt-dlp and Deno independently: a Deno failure never blocks the yt-dlp
/// update (and vice versa). Both outcomes are reported; any failure exits non-zero.
fn yt_dlp_update(requested_version: Option<&str>) -> Result<()> {
    let report = update_tools(
        requested_version,
        concat!("archivr-cli/", env!("CARGO_PKG_VERSION")),
        false,
        &mut |l| println!("{l}"),
    )?;

    println!("\nSummary:");
    match &report.yt_dlp {
        Ok(_) => println!("  yt-dlp: ok"),
        Err(e) => println!("  yt-dlp: FAILED: {e:#}"),
    }
    match &report.deno {
        Ok(msg) => println!("  deno: {msg}"),
        Err(e) => println!("  deno: FAILED: {e:#}"),
    }

    let failed = report.failed_components();
    if !failed.is_empty() {
        bail!("update failed for: {}", failed.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{candidate_row, format_status_row};
    use archivr_core::downloader::ytdlp_tools::ToolCandidate;
    use archivr_core::downloader::ytdlp::{
        forced_yt_dlp, probe_version, resolve_yt_dlp_uncached, YT_DLP_FORCE_ENV,
    };
    use std::path::Path;

    /// Writes an executable script to `path` (callers must use a fresh path each time), then
    /// waits until it can be exec'd. A child forked by a parallel test while our write fd was
    /// open keeps a copy of it until that child execs, so our own exec can fail with ETXTBSY
    /// (rust-lang/rust#114554). One exec that isn't ETXTBSY proves no writer is left, and none
    /// can appear later because our fd is already closed.
    #[cfg(unix)]
    fn write_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(path).arg("--version").output() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                _ => return,
            }
        }
        panic!("{} stayed busy (ETXTBSY)", path.display());
    }

    #[cfg(unix)]
    #[test]
    fn forced_candidate_is_rendered_and_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let forced = tmp.path().join("forced/yt-dlp");
        write_script(&forced, "#!/bin/sh\necho 2020.01.01\n");
        unsafe { std::env::set_var(YT_DLP_FORCE_ENV, &forced) };

        let candidate = forced_yt_dlp();
        assert_eq!(candidate.as_deref(), Some(forced.as_path()));
        let chosen = resolve_yt_dlp_uncached();
        assert_eq!(chosen, forced);
        let version = probe_version(&forced);
        assert_eq!(
            format_status_row(
                "force (ARCHIVR_YT_DLP_FORCE)",
                Some(&forced.display().to_string()),
                version.as_deref(),
                chosen == forced,
            ),
            format!(
                "force (ARCHIVR_YT_DLP_FORCE)\t{}\t2020.01.01\t*",
                forced.display()
            )
        );

        unsafe { std::env::remove_var(YT_DLP_FORCE_ENV) };
    }

    #[test]
    fn missing_candidate_renders_dashes() {
        assert_eq!(
            format_status_row("state-dir", None, Some("2.9.7"), true),
            "state-dir\t—\t—\t"
        );
    }

    #[test]
    fn unknown_version_renders_dash() {
        assert_eq!(
            format_status_row("path (deno)", Some("/bin/deno"), None, false),
            "path (deno)\t/bin/deno\t—\t"
        );
    }

    #[test]
    fn invalid_override_row_shows_reason_and_is_not_chosen() {
        assert_eq!(
            format_status_row(
                "force (ARCHIVR_JS_RUNTIME)",
                Some("python"),
                Some("invalid: unknown runtime python (expected deno, node, bun or quickjs)"),
                false,
            ),
            "force (ARCHIVR_JS_RUNTIME)\tpython\tinvalid: unknown runtime python \
             (expected deno, node, bun or quickjs)\t"
        );
    }

    #[test]
    fn candidate_row_renders_invalid_override() {
        let c = ToolCandidate {
            role: "force",
            label: "force (ARCHIVR_JS_RUNTIME)",
            path: Some("python".into()),
            version: None,
            chosen: false,
            invalid: Some("unknown runtime python (expected deno, node, bun or quickjs)".into()),
        };
        assert_eq!(
            candidate_row(&c),
            "force (ARCHIVR_JS_RUNTIME)\tpython\tinvalid: unknown runtime python \
             (expected deno, node, bun or quickjs)\t"
        );
    }
}
