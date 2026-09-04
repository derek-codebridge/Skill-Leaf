use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use skillleaf::{
    BuildOptions, DEFAULT_MAX_FILE_BYTES, DEFAULT_SYNC_CHUNK_BYTES, DomainConfig, EntryKind,
    HostKind, PullOptions, SourceRoot, TrustLevel, apply_migration_with_receipt,
    build_catalog_with_options, doctor, domain_catalog_path, evaluate, hydrate_with_policy,
    inspect_catalog, load_catalog, load_domain_registry, load_eval_suite, load_migration_plan,
    load_migration_receipt, plan_migration, publish_snapshot, pull_snapshot, record_hydrations,
    resolve, rollback_migration, sync_status, usage_report, write_catalog_atomic,
    write_domain_registry_atomic, write_migration_plan,
};
use std::collections::BTreeSet;
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
        #[arg(long = "untrusted-skills", value_name = "NAME=PATH")]
        untrusted_skills: Vec<String>,
        #[arg(long = "untrusted-commands", value_name = "NAME=PATH")]
        untrusted_commands: Vec<String>,
        #[arg(short, long, default_value = "skillleaf.json")]
        output: PathBuf,
        #[arg(long, default_value_t = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// Select a bounded skill/command dependency closure for a task.
    Resolve {
        #[command(flatten)]
        selection: CatalogSelection,
        #[arg(long)]
        task: String,
        #[arg(long = "require")]
        required: Vec<String>,
        #[arg(long, default_value_t = 3)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Hydrate one or more selected bodies in a single process.
    Read {
        #[command(flatten)]
        selection: CatalogSelection,
        #[arg(long = "many", value_delimiter = ',', required = true)]
        selectors: Vec<String>,
        /// Optional local ledger. No prompts, task text, or source paths are recorded.
        #[arg(long, env = "SKILLLEAF_USAGE_FILE")]
        usage_file: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        /// Explicitly permit hydration from a source indexed as untrusted.
        #[arg(long)]
        allow_untrusted: bool,
    },
    /// Verify the catalog, every body hash, dependency, and containment boundary.
    Doctor {
        #[command(flatten)]
        selection: CatalogSelection,
    },
    /// Report local hydration counts, including catalogue entries never hydrated.
    Stats {
        #[command(flatten)]
        selection: CatalogSelection,
        #[arg(long, env = "SKILLLEAF_USAGE_FILE")]
        usage_file: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Evaluate routing against deterministic JSON fixtures.
    Eval {
        #[command(flatten)]
        selection: CatalogSelection,
        #[arg(long)]
        suite: PathBuf,
        #[arg(long, default_value_t = 3)]
        limit: usize,
    },
    /// Inspect catalogue trust and declared-capability policy.
    Inspect {
        #[command(flatten)]
        selection: CatalogSelection,
        #[arg(long)]
        strict: bool,
    },
    /// Manage isolated catalogue domains. Domains are never merged during routing.
    Domain {
        #[command(subcommand)]
        command: DomainCommand,
    },
    /// Plan, apply, or roll back a non-destructive library migration.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Publish, update, or inspect a filesystem-backed shared snapshot.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
}

#[derive(Clone, Debug, Args)]
struct CatalogSelection {
    #[arg(short, long, env = "SKILLLEAF_CATALOG")]
    catalog: Option<PathBuf>,
    #[arg(long, env = "SKILLLEAF_DOMAIN")]
    domain: Option<String>,
    #[arg(
        long,
        env = "SKILLLEAF_REGISTRY",
        default_value = "skillleaf-domains.json"
    )]
    registry: PathBuf,
}

#[derive(Subcommand)]
enum DomainCommand {
    Add {
        name: String,
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        usage_file: Option<PathBuf>,
        #[arg(long)]
        account: Option<String>,
        #[arg(
            long,
            env = "SKILLLEAF_REGISTRY",
            default_value = "skillleaf-domains.json"
        )]
        registry: PathBuf,
    },
    List {
        #[arg(
            long,
            env = "SKILLLEAF_REGISTRY",
            default_value = "skillleaf-domains.json"
        )]
        registry: PathBuf,
    },
    Remove {
        name: String,
        #[arg(
            long,
            env = "SKILLLEAF_REGISTRY",
            default_value = "skillleaf-domains.json"
        )]
        registry: PathBuf,
    },
}

#[derive(Subcommand)]
enum MigrateCommand {
    Plan {
        #[arg(long = "skills", value_name = "NAME=PATH")]
        skills: Vec<String>,
        #[arg(long = "commands", value_name = "NAME=PATH")]
        commands: Vec<String>,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        domain: String,
        #[arg(
            long,
            env = "SKILLLEAF_REGISTRY",
            default_value = "skillleaf-domains.json"
        )]
        registry: PathBuf,
        #[arg(long, value_enum)]
        host: HostArgument,
        #[arg(long)]
        host_root: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    Apply {
        #[arg(long)]
        plan: PathBuf,
        #[arg(long)]
        receipt: PathBuf,
    },
    Rollback {
        #[arg(long)]
        receipt: PathBuf,
    },
}

#[derive(Subcommand)]
enum SyncCommand {
    /// Publish the indexed Markdown bodies as an immutable chunked snapshot.
    Publish {
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        remote: PathBuf,
        #[arg(long, default_value_t = DEFAULT_SYNC_CHUNK_BYTES)]
        chunk_bytes: usize,
    },
    /// Pull and atomically bind the current snapshot to an isolated domain.
    Pull {
        #[arg(long)]
        remote: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        domain: String,
        #[arg(
            long,
            env = "SKILLLEAF_REGISTRY",
            default_value = "skillleaf-domains.json"
        )]
        registry: PathBuf,
        #[arg(long, conflicts_with = "trust_remote")]
        expected_snapshot: Option<String>,
        /// Preserve publisher trust metadata without a snapshot pin.
        #[arg(long)]
        trust_remote: bool,
        /// Fail instead of rebinding the last verified local snapshot when remote storage is unavailable.
        #[arg(long)]
        no_offline_fallback: bool,
    },
    /// Report remote freshness and whether a verified local fallback is ready.
    Status {
        #[arg(long)]
        remote: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        domain: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum HostArgument {
    Claude,
    Codex,
    OpenCode,
    Generic,
}

impl From<HostArgument> for HostKind {
    fn from(value: HostArgument) -> Self {
        match value {
            HostArgument::Claude => Self::Claude,
            HostArgument::Codex => Self::Codex,
            HostArgument::OpenCode => Self::OpenCode,
            HostArgument::Generic => Self::Generic,
        }
    }
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
            untrusted_skills,
            untrusted_commands,
            output,
            max_file_bytes,
        } => run_index(
            skills,
            commands,
            untrusted_skills,
            untrusted_commands,
            output,
            max_file_bytes,
        )?,
        Command::Resolve {
            selection,
            task,
            required,
            limit,
            format,
        } => run_resolve(selection, task, required, limit, format)?,
        Command::Read {
            selection,
            selectors,
            usage_file,
            format,
            allow_untrusted,
        } => run_read(selection, selectors, usage_file, format, allow_untrusted)?,
        Command::Doctor { selection } => {
            let catalog = selected_catalog(&selection)?;
            let catalog = load_catalog(&catalog)?;
            doctor(&catalog)?;
            println!("PASS: {} entries verified", catalog.entries.len());
        }
        Command::Stats {
            selection,
            usage_file,
            format,
        } => run_stats(selection, usage_file, format)?,
        Command::Eval {
            selection,
            suite,
            limit,
        } => {
            let catalog = load_catalog(&selected_catalog(&selection)?)?;
            let report = evaluate(&catalog, &load_eval_suite(&suite)?, limit)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                bail!("routing evaluation failed");
            }
        }
        Command::Inspect { selection, strict } => {
            let catalog = load_catalog(&selected_catalog(&selection)?)?;
            let report = inspect_catalog(&catalog, strict)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                bail!("catalogue policy inspection failed");
            }
        }
        Command::Domain { command } => run_domain(command)?,
        Command::Migrate { command } => run_migrate(command)?,
        Command::Sync { command } => run_sync(command)?,
    }
    Ok(())
}

fn run_index(
    skills: Vec<String>,
    commands: Vec<String>,
    untrusted_skills: Vec<String>,
    untrusted_commands: Vec<String>,
    output: PathBuf,
    max_file_bytes: u64,
) -> Result<()> {
    let roots = skills
        .into_iter()
        .map(|value| parse_source(value, EntryKind::Skill))
        .chain(
            commands
                .into_iter()
                .map(|value| parse_source(value, EntryKind::Command)),
        )
        .chain(
            untrusted_skills
                .iter()
                .cloned()
                .map(|value| parse_source(value, EntryKind::Skill)),
        )
        .chain(
            untrusted_commands
                .iter()
                .cloned()
                .map(|value| parse_source(value, EntryKind::Command)),
        )
        .collect::<Result<Vec<_>>>()?;
    let untrusted_roots = untrusted_skills
        .iter()
        .filter_map(|value| {
            value
                .split_once('=')
                .map(|(name, _)| format!("{name}/skill"))
        })
        .chain(untrusted_commands.iter().filter_map(|value| {
            value
                .split_once('=')
                .map(|(name, _)| format!("{name}/command"))
        }))
        .collect::<BTreeSet<_>>();
    let catalog = build_catalog_with_options(
        &roots,
        max_file_bytes,
        &BuildOptions {
            untrusted_roots,
            ..BuildOptions::default()
        },
    )?;
    write_catalog_atomic(&catalog, &output)?;
    println!(
        "indexed {} entries into {} ({})",
        catalog.entries.len(),
        output.display(),
        catalog.catalog_sha256
    );
    Ok(())
}

fn run_resolve(
    selection: CatalogSelection,
    task: String,
    required: Vec<String>,
    limit: usize,
    format: OutputFormat,
) -> Result<()> {
    let resolution = resolve(
        &load_catalog(&selected_catalog(&selection)?)?,
        &task,
        &required,
        limit,
    )?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&resolution)?),
        OutputFormat::Text => {
            for entry in resolution.selected {
                let trust = if entry.trust == TrustLevel::Untrusted {
                    "\tUNTRUSTED"
                } else {
                    ""
                };
                println!("{}\t{}{}", entry.selector, entry.description, trust);
            }
        }
    }
    Ok(())
}

fn run_read(
    selection: CatalogSelection,
    selectors: Vec<String>,
    usage_file: Option<PathBuf>,
    format: OutputFormat,
    allow_untrusted: bool,
) -> Result<()> {
    let hydrated = hydrate_with_policy(
        &load_catalog(&selected_catalog(&selection)?)?,
        &selectors,
        allow_untrusted,
    )?;
    if let Some(path) = usage_file {
        record_hydrations(&path, &hydrated)?;
    }
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&hydrated)?),
        OutputFormat::Text => {
            for entry in hydrated {
                let trust = if entry.trust == TrustLevel::Untrusted {
                    " UNTRUSTED"
                } else {
                    ""
                };
                println!(
                    "--- {} {}{} ---",
                    entry.selector, entry.content_sha256, trust
                );
                println!("{}", entry.content);
            }
        }
    }
    Ok(())
}

fn run_stats(selection: CatalogSelection, usage_file: PathBuf, format: OutputFormat) -> Result<()> {
    let report = usage_report(&load_catalog(&selected_catalog(&selection)?)?, &usage_file)?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        OutputFormat::Text => {
            for entry in report.entries {
                println!("{}\t{}", entry.hydrate_count, entry.selector);
            }
        }
    }
    Ok(())
}

fn selected_catalog(selection: &CatalogSelection) -> Result<PathBuf> {
    domain_catalog_path(
        selection.catalog.clone(),
        selection.domain.as_deref(),
        &selection.registry,
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn run_domain(command: DomainCommand) -> Result<()> {
    match command {
        DomainCommand::Add {
            name,
            catalog,
            usage_file,
            account,
            registry,
        } => {
            let mut domains = load_domain_registry(&registry)?;
            if domains.domains.contains_key(&name) {
                bail!("domain already exists: {name}");
            }
            doctor(&load_catalog(&catalog)?)?;
            domains.domains.insert(
                name.clone(),
                DomainConfig {
                    catalog,
                    usage_file,
                    account,
                },
            );
            write_domain_registry_atomic(&domains, &registry)?;
            println!("added isolated domain {name}");
        }
        DomainCommand::List { registry } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&load_domain_registry(&registry)?)?
            );
        }
        DomainCommand::Remove { name, registry } => {
            let mut domains = load_domain_registry(&registry)?;
            if domains.domains.remove(&name).is_none() {
                bail!("unknown domain: {name}");
            }
            write_domain_registry_atomic(&domains, &registry)?;
            println!("removed domain {name}; catalogue files were not deleted");
        }
    }
    Ok(())
}

fn run_migrate(command: MigrateCommand) -> Result<()> {
    match command {
        MigrateCommand::Plan {
            skills,
            commands,
            destination,
            domain,
            registry,
            host,
            host_root,
            output,
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
            let plan = plan_migration(
                &roots,
                destination,
                domain,
                registry,
                host.into(),
                host_root,
            )?;
            write_migration_plan(&plan, &output)?;
            println!("planned migration {} in {}", plan.plan_id, output.display());
        }
        MigrateCommand::Apply { plan, receipt } => {
            let receipt_data =
                apply_migration_with_receipt(&load_migration_plan(&plan)?, &receipt)?;
            println!(
                "applied migration {}; receipt {}",
                receipt_data.plan_id,
                receipt.display()
            );
        }
        MigrateCommand::Rollback { receipt } => {
            let receipt_data = load_migration_receipt(&receipt)?;
            rollback_migration(&receipt_data)?;
            println!("rolled back migration {}", receipt_data.plan_id);
        }
    }
    Ok(())
}

fn run_sync(command: SyncCommand) -> Result<()> {
    match command {
        SyncCommand::Publish {
            catalog,
            remote,
            chunk_bytes,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&publish_snapshot(&catalog, &remote, chunk_bytes)?)?
        ),
        SyncCommand::Pull {
            remote,
            destination,
            domain,
            registry,
            expected_snapshot,
            trust_remote,
            no_offline_fallback,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&pull_snapshot(
                &remote,
                &destination,
                &registry,
                &domain,
                &PullOptions {
                    expected_snapshot,
                    trust_remote,
                    allow_offline_fallback: !no_offline_fallback,
                },
            )?)?
        ),
        SyncCommand::Status {
            remote,
            destination,
            domain,
        } => println!(
            "{}",
            serde_json::to_string_pretty(&sync_status(&remote, &destination, &domain)?)?
        ),
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
