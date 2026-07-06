use anyhow::{Context, Result};
use archivr_core::{archive, capture::CaptureConfig};
use clap::{Parser, Subcommand};
use std::{
    env,
    path::Path,
    process,
};

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
        } // _ => eprintln!("Unknown command: {:?}", args.command),
    }
}

