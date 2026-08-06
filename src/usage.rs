use crate::{Catalog, EntryKind, HydratedEntry, SkillleafError, SkillleafResult};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE_SCHEMA: &str = "skillleaf.usage.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UsageLedger {
    schema: String,
    entries: BTreeMap<String, UsageRecord>,
}

impl Default for UsageLedger {
    fn default() -> Self {
        Self {
            schema: USAGE_SCHEMA.to_string(),
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct UsageRecord {
    hydrate_count: u64,
    last_hydrated_unix_ms: u64,
    content_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageReport {
    pub schema: &'static str,
    pub entries: Vec<UsageEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageEntry {
    pub selector: String,
    pub kind: EntryKind,
    pub hydrate_count: u64,
    pub last_hydrated_unix_ms: Option<u64>,
    pub catalog_content_sha256: String,
    pub last_hydrated_content_sha256: Option<String>,
    pub content_changed_since_last_hydration: bool,
}

pub fn record_hydrations(path: &Path, hydrated: &[HydratedEntry]) -> SkillleafResult<()> {
    record_hydrations_inner(path, hydrated)
        .map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

fn record_hydrations_inner(path: &Path, hydrated: &[HydratedEntry]) -> Result<()> {
    if hydrated.is_empty() {
        return Ok(());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("cannot create usage directory {}", parent.display()))?;
    let lock_path = lock_path(path);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("cannot open usage lock {}", lock_path.display()))?;
    lock.lock_exclusive()
        .with_context(|| format!("cannot lock usage ledger {}", path.display()))?;

    let result = (|| {
        let mut ledger = load_ledger(path)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_millis()
            .try_into()
            .context("usage timestamp exceeds u64")?;
        for entry in hydrated {
            let record = ledger
                .entries
                .entry(entry.selector.clone())
                .or_insert_with(|| UsageRecord {
                    hydrate_count: 0,
                    last_hydrated_unix_ms: now,
                    content_sha256: entry.content_sha256.clone(),
                });
            record.hydrate_count = record.hydrate_count.saturating_add(1);
            record.last_hydrated_unix_ms = now;
            record.content_sha256.clone_from(&entry.content_sha256);
        }
        write_ledger_atomic(path, &ledger)
    })();
    let unlock_result = FileExt::unlock(&lock).context("cannot unlock usage ledger");
    result.and(unlock_result)
}

pub fn usage_report(catalog: &Catalog, path: &Path) -> SkillleafResult<UsageReport> {
    usage_report_inner(catalog, path)
        .map_err(|error| SkillleafError::Integrity(format!("{error:#}")))
}

fn usage_report_inner(catalog: &Catalog, path: &Path) -> Result<UsageReport> {
    let ledger = load_ledger(path)?;
    let mut entries = catalog
        .entries
        .iter()
        .filter(|entry| entry.kind != EntryKind::Resource)
        .map(|entry| {
            let selector = entry.selector();
            let record = ledger.entries.get(&selector);
            UsageEntry {
                selector,
                kind: entry.kind,
                hydrate_count: record.map_or(0, |value| value.hydrate_count),
                last_hydrated_unix_ms: record.map(|value| value.last_hydrated_unix_ms),
                catalog_content_sha256: entry.content_sha256.clone(),
                last_hydrated_content_sha256: record.map(|value| value.content_sha256.clone()),
                content_changed_since_last_hydration: record
                    .is_some_and(|value| value.content_sha256 != entry.content_sha256),
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .hydrate_count
            .cmp(&left.hydrate_count)
            .then_with(|| left.selector.cmp(&right.selector))
    });
    Ok(UsageReport {
        schema: "skillleaf.usage-report.v1",
        entries,
    })
}

fn load_ledger(path: &Path) -> Result<UsageLedger> {
    if !path.exists() {
        return Ok(UsageLedger::default());
    }
    let bytes =
        fs::read(path).with_context(|| format!("cannot read usage ledger {}", path.display()))?;
    let ledger: UsageLedger = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid usage ledger JSON in {}", path.display()))?;
    if ledger.schema != USAGE_SCHEMA {
        bail!("unsupported usage schema: {}", ledger.schema);
    }
    Ok(ledger)
}

fn write_ledger_atomic(path: &Path, ledger: &UsageLedger) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, ledger)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("cannot atomically replace {}", path.display()))?;
    Ok(())
}

fn lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}
