use anyhow::Result;
use chrono::Local;
use clap::{Parser, Subcommand};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

mod downloader;
mod hash;

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
        ///   raw/
        ///     ...
        ///   structured/
        ///     ...
        #[arg(default_value = "./.archivr/store")]
        store_path: String,

        /// Name of the archive
        #[arg(short, long)]
        name: String,
    },
}

fn get_archive_path() -> Option<PathBuf> {
    let mut dir = env::current_dir().unwrap();
    loop {
        if dir.join(".archivr").is_dir() {
            return Some(dir.join(".archivr"));
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[derive(Debug)]
enum Source {
    YouTube,
    Other,
}

fn determine_source(path: &str) -> Source {
    if path.starts_with("http://") || path.starts_with("https://") {
        return Source::YouTube;
    }
    Source::Other
}

fn hash_exists(filename: String, store_path: &Path) -> bool {
    let mut chars = filename.chars();
    let first_letter = chars.next().unwrap();
    let second_letter = chars.next().unwrap();

    let path = store_path
        .join("raw")
        .join(first_letter.to_string())
        .join(second_letter.to_string())
        .join(filename);

    println!("Checking {}", path.display());

    path.exists()
}

fn move_temp_to_raw(file: &Path, hash: &String, store_path: &Path) -> Result<()> {
    let mut chars = hash.chars();
    let first_letter = chars.next().unwrap().to_string();
    let second_letter = chars.next().unwrap().to_string();
    let file_extension = file
        .extension()
        .map_or(String::new(), |ext| format!(".{}", ext.to_string_lossy()));

    fs::create_dir_all(
        store_path
            .join("raw")
            .join(&first_letter)
            .join(&second_letter),
    )?;

    fs::rename(
        file,
        store_path
            .join("raw")
            .join(&first_letter)
            .join(&second_letter)
            .join(format!(
                "{hash}{}",
                if file_extension.is_empty() {
                    ""
                } else {
                    &file_extension
                }
            )),
    )?;

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Archive { ref path } => {
            let archive_path = get_archive_path();
            if get_archive_path().is_none() {
                eprintln!("Not in an archive. Use 'archivr init' to create one.");
                process::exit(1);
            }

            // let download_id = uuid::Uuid::new_v4();
            let timestamp = Local::now().format("%Y-%m-%dT%H-%M-%S%.3f").to_string();

            let source = determine_source(path);
            if let Source::YouTube = source {
                let store_path_string_file = archive_path.unwrap().join("store_path");
                let store_path = match fs::read_to_string(store_path_string_file) {
                    Ok(p) => PathBuf::from(p.trim()),
                    Err(e) => {
                        eprintln!("Failed to read store path: {e}");
                        process::exit(1);
                    }
                };

                let hash =
                    match downloader::youtube::download(path.clone(), &store_path, &timestamp) {
                        Ok(h) => h,
                        Err(e) => {
                            eprintln!("Failed to download from YouTube: {e}");
                            process::exit(1);
                        }
                    };

                let hash_exists = hash_exists(format!("{hash}.mp4"), &store_path);
                // TODO: check for repeated archives?
                // There could be one of the following:
                // - We are literally archiving the same path over again.
                // - We are archiving a different path, which had this file. E.g.: we archived a
                // website before which had this YouTube video, and while recursively archiving
                // everything, we also archived the YouTube video although it wasn't our main
                // target. This means that we should archive again; whereas with the first case...
                // Not sure. Need to think about this.
                // ----
                // Thinking about it a day later...
                // If we are specifically archiving a YouTube video, it could also be two of the
                // above. So yeah, just create a new DB entry and symlink the Raw to the Structured
                // Dir or whatever. it's midnight and my brain ain't wording/braining.
                if hash_exists {
                    println!("File already archived.");
                    process::exit(0);
                } else {
                    move_temp_to_raw(
                        &store_path.join("temp").join(format!("{timestamp}.mp4")),
                        &hash,
                        &store_path,
                    )?;

                    println!("File archived successfully.");
                }
            }

            // TODO: DB INSERT, inserting a record

            Ok(())
        }

        Command::Init {
            path: ref archive_path_string,
            store_path: ref store_path_string,
            name: ref archive_name,
        } => {
            let archive_path = Path::new(&archive_path_string).join(".archivr");
            let store_path = if Path::new(&store_path_string).is_relative() {
                env::current_dir().unwrap().join(store_path_string)
            } else {
                Path::new(store_path_string).to_path_buf()
            };

            if archive_path.exists() {
                // TODO: check if there is nothing inside. if there is nothing inside, use it
                eprintln!("Archive already exists at {}", archive_path.display());
                if store_path.exists() {
                    eprintln!("Store path already exists at {}", store_path.display());
                    process::exit(1);
                }
                process::exit(1);
            }
            if store_path.exists() {
                // TODO: check if the structure is correct. If so, use it.
                eprintln!("Store path already exists at {}", store_path.display());
                process::exit(1);
            }

            fs::create_dir_all(&archive_path).unwrap();
            fs::create_dir_all(&store_path).unwrap();
            fs::write(archive_path.join("name"), archive_name).unwrap();
            let _ = fs::write(
                archive_path.join("store_path"),
                store_path.canonicalize().unwrap().to_str().unwrap(),
            );
            fs::create_dir_all(store_path.join("raw")).unwrap();
            fs::create_dir_all(store_path.join("structured")).unwrap();
            fs::create_dir_all(store_path.join("tmp")).unwrap();

            println!("Initialized empty archive in {}", archive_path.display());

            Ok(())
        } // _ => eprintln!("Unknown command: {:?}", args.command),
    }
}
