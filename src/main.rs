use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use skillleaf::{
    DEFAULT_MAX_FILE_BYTES, EntryKind, SourceRoot, build_catalog, doctor, hydrate, load_catalog,
    record_hydrations, resolve, usage_report, write_catalog_atomic,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Route agent skills without loading the whole library"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build a deterministic, hash-verified catalog.
    Index {
        #[arg(long = "skills", value_name = "NAME=PATH")]
        skills: Vec<String>,
        #[arg(long = "commands", value_name = "NAME=PATH")]
        commands: Vec<String>,
        #[arg(short, long, default_value = "skillleaf.json")]
        output: PathBuf,
        #[arg(long, default_value_t = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// Select a bounded skill/command dependency closure for a task.
    Resolve {
        #[arg(
            short,
            long,
            env = "SKILLLEAF_CATALOG",
            default_value = "skillleaf.json"
        )]
        catalog: PathBuf,
        #[arg(long)]
        task: String,
        #[arg(long = "require")]
        required: Vec<String>,
        #[arg(long, default_value_t = 8)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Hydrate one or more selected bodies in a single process.
    Read {
        #[arg(
            short,
            long,
            env = "SKILLLEAF_CATALOG",
            default_value = "skillleaf.json"
        )]
        catalog: PathBuf,
        #[arg(long = "many", value_delimiter = ',', required = true)]
        selectors: Vec<String>,
        /// Optional local ledger. No prompts, task text, or source paths are recorded.
        #[arg(long, env = "SKILLLEAF_USAGE_FILE")]
        usage_file: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Verify the catalog, every body hash, dependency, and containment boundary.
    Doctor {
        #[arg(
            short,
            long,
            env = "SKILLLEAF_CATALOG",
            default_value = "skillleaf.json"
        )]
        catalog: PathBuf,
    },
    /// Report local hydration counts, including catalogue entries never hydrated.
    Stats {
        #[arg(
            short,
            long,
            env = "SKILLLEAF_CATALOG",
            default_value = "skillleaf.json"
        )]
        catalog: PathBuf,
        #[arg(long, env = "SKILLLEAF_USAGE_FILE")]
        usage_file: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Text,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("skillleaf: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    match Cli::parse().command {
        Command::Index {
            skills,
            commands,
            output,
            max_file_bytes,
        } => {
            let roots = skills
                .into_iter()
                .map(|value| parse_source(value, EntryKind::Skill))
                .chain(
                    commands
                        .into_iter()
                        .map(|value| parse_source(value, EntryKind::Command)),
                )
                .collect::<Result<Vec<_>>>()?;
            let catalog = build_catalog(&roots, max_file_bytes)?;
            write_catalog_atomic(&catalog, &output)?;
            println!(
                "indexed {} entries into {} ({})",
                catalog.entries.len(),
                output.display(),
                catalog.catalog_sha256
            );
        }
        Command::Resolve {
            catalog,
            task,
            required,
            limit,
            format,
        } => {
            let resolution = resolve(&load_catalog(&catalog)?, &task, &required, limit)?;
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&resolution)?),
                OutputFormat::Text => {
                    for entry in resolution.selected {
                        println!("{}\t{}", entry.selector, entry.description);
                    }
                }
            }
        }
        Command::Read {
            catalog,
            selectors,
            usage_file,
            format,
        } => {
            let hydrated = hydrate(&load_catalog(&catalog)?, &selectors)?;
            if let Some(path) = usage_file {
                record_hydrations(&path, &hydrated)?;
            }
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&hydrated)?),
                OutputFormat::Text => {
                    for entry in hydrated {
                        println!("--- {} {} ---", entry.selector, entry.content_sha256);
                        println!("{}", entry.content);
                    }
                }
            }
        }
        Command::Doctor { catalog } => {
            let catalog = load_catalog(&catalog)?;
            doctor(&catalog)?;
            println!("PASS: {} entries verified", catalog.entries.len());
        }
        Command::Stats {
            catalog,
            usage_file,
            format,
        } => {
            let report = usage_report(&load_catalog(&catalog)?, &usage_file)?;
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                OutputFormat::Text => {
                    for entry in report.entries {
                        println!("{}\t{}", entry.hydrate_count, entry.selector);
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_source(value: String, kind: EntryKind) -> Result<SourceRoot> {
    let (name, path) = value
        .split_once('=')
        .with_context(|| format!("source must use NAME=PATH: {value}"))?;
    if name.is_empty() || path.is_empty() {
        bail!("source must use a non-empty NAME=PATH: {value}");
    }
    Ok(SourceRoot {
        name: name.to_string(),
        kind,
        path: PathBuf::from(path),
    })
}
