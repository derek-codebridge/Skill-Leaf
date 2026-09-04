use crate::domain::{DomainConfig, DomainRegistry, validate_domain_name};
use crate::{
    Catalog, EntryKind, SkillleafError, SkillleafResult, SourceRoot, build_catalog, doctor, sha256,
    write_catalog_atomic, write_domain_registry_atomic, write_json_atomic,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const PLAN_SCHEMA: &str = "skillleaf.migration-plan.v1";
const RECEIPT_SCHEMA: &str = "skillleaf.migration-receipt.v1";
const MAX_MIGRATION_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Claude,
    Codex,
    OpenCode,
    Generic,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationSource {
    pub name: String,
    pub kind: EntryKind,
    pub path: PathBuf,
    pub manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationPlan {
    pub schema: String,
    pub plan_id: String,
    pub domain: String,
    pub destination_root: PathBuf,
    pub registry_path: PathBuf,
    pub host: HostKind,
    pub host_root: PathBuf,
    pub sources: Vec<MigrationSource>,
    pub preserves_native_commands: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MigrationReceipt {
    pub schema: String,
    pub plan_id: String,
    pub domain: String,
    pub domain_root: PathBuf,
    pub domain_manifest_sha256: String,
    pub catalog_sha256: String,
    pub registry_path: PathBuf,
    pub registry_before: Option<DomainRegistry>,
    pub registry_after_sha256: String,
    pub adapter_path: PathBuf,
    pub adapter_created: bool,
    pub adapter_sha256: String,
}

pub fn plan_migration(
    roots: &[SourceRoot],
    destination_root: PathBuf,
    domain: String,
    registry_path: PathBuf,
    host: HostKind,
    host_root: PathBuf,
) -> SkillleafResult<MigrationPlan> {
    plan_migration_inner(
        roots,
        destination_root,
        domain,
        registry_path,
        host,
        host_root,
    )
    .map_err(|error| SkillleafError::CatalogInput(format!("{error:#}")))
}

fn plan_migration_inner(
    roots: &[SourceRoot],
    destination_root: PathBuf,
    domain: String,
    registry_path: PathBuf,
    host: HostKind,
    host_root: PathBuf,
) -> Result<MigrationPlan> {
    validate_domain_name(&domain)?;
    if roots.is_empty() {
        bail!("migration requires at least one skill or command source");
    }
    let mut sources = Vec::new();
    for root in roots {
        let path = root
            .path
            .canonicalize()
            .with_context(|| format!("cannot resolve migration source {}", root.path.display()))?;
        if !path.is_dir() {
            bail!("migration source is not a directory: {}", path.display());
        }
        sources.push(MigrationSource {
            name: root.name.clone(),
            kind: root.kind,
            manifest_sha256: directory_manifest_hash(&path)?,
            path,
        });
    }
    sources.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.cmp(&right.name))
    });
    let destination_root = absolute_without_existing(&destination_root)?;
    let registry_path = absolute_without_existing(&registry_path)?;
    let host_root = absolute_without_existing(&host_root)?;
    for source in &sources {
        if destination_root.starts_with(&source.path) {
            bail!(
                "migration destination cannot be inside a source root: {}",
                source.path.display()
            );
        }
        if registry_path.starts_with(&source.path) {
            bail!(
                "domain registry cannot be inside a source root: {}",
                source.path.display()
            );
        }
    }
    let mut plan = MigrationPlan {
        schema: PLAN_SCHEMA.to_string(),
        plan_id: String::new(),
        domain,
        destination_root,
        registry_path,
        host,
        host_root,
        sources,
        preserves_native_commands: true,
    };
    plan.plan_id = computed_plan_id(&plan)?;
    Ok(plan)
}

pub fn write_migration_plan(plan: &MigrationPlan, path: &Path) -> SkillleafResult<()> {
    write_json_atomic(plan, path).map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

pub fn write_migration_receipt(receipt: &MigrationReceipt, path: &Path) -> SkillleafResult<()> {
    write_json_atomic(receipt, path).map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

pub fn load_migration_plan(path: &Path) -> SkillleafResult<MigrationPlan> {
    let bytes = fs::read(path).map_err(|error| {
        SkillleafError::Integrity(format!("cannot read {}: {error}", path.display()))
    })?;
    let plan: MigrationPlan = serde_json::from_slice(&bytes)
        .map_err(|error| SkillleafError::Integrity(format!("invalid migration plan: {error}")))?;
    if plan.schema != PLAN_SCHEMA {
        return Err(SkillleafError::Integrity(format!(
            "unsupported migration plan schema: {}",
            plan.schema
        )));
    }
    validate_plan(&plan).map_err(|error| SkillleafError::Integrity(format!("{error:#}")))?;
    Ok(plan)
}

pub fn load_migration_receipt(path: &Path) -> SkillleafResult<MigrationReceipt> {
    let bytes = fs::read(path).map_err(|error| {
        SkillleafError::Integrity(format!("cannot read {}: {error}", path.display()))
    })?;
    let receipt: MigrationReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        SkillleafError::Integrity(format!("invalid migration receipt: {error}"))
    })?;
    if receipt.schema != RECEIPT_SCHEMA {
        return Err(SkillleafError::Integrity(format!(
            "unsupported migration receipt schema: {}",
            receipt.schema
        )));
    }
    Ok(receipt)
}

pub fn apply_migration(plan: &MigrationPlan) -> SkillleafResult<MigrationReceipt> {
    apply_migration_inner(plan).map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

pub fn apply_migration_with_receipt(
    plan: &MigrationPlan,
    receipt_path: &Path,
) -> SkillleafResult<MigrationReceipt> {
    let receipt = apply_migration(plan)?;
    if let Err(write_error) = write_migration_receipt(&receipt, receipt_path) {
        if let Err(rollback_error) = rollback_migration(&receipt) {
            return Err(SkillleafError::Storage(format!(
                "cannot write migration receipt ({write_error}); automatic rollback also failed ({rollback_error})"
            )));
        }
        return Err(SkillleafError::Storage(format!(
            "cannot write migration receipt; migration was rolled back: {write_error}"
        )));
    }
    Ok(receipt)
}

fn apply_migration_inner(plan: &MigrationPlan) -> Result<MigrationReceipt> {
    validate_plan(plan)?;
    validate_domain_name(&plan.domain)?;
    for source in &plan.sources {
        let current = directory_manifest_hash(&source.path)?;
        if current != source.manifest_sha256 {
            bail!(
                "migration source changed after planning: {}",
                source.path.display()
            );
        }
    }
    let domains_root = plan.destination_root.join("domains");
    fs::create_dir_all(&domains_root)?;
    let domains_root = domains_root.canonicalize()?;
    let domain_root = domains_root.join(&plan.domain);
    if domain_root.exists() {
        bail!(
            "domain destination already exists: {}",
            domain_root.display()
        );
    }
    let staging_root = domains_root.join(format!(
        ".skillleaf-{}-{}.staging",
        plan.domain, plan.plan_id
    ));
    if staging_root.exists() {
        bail!(
            "migration staging destination already exists: {}",
            staging_root.display()
        );
    }
    fs::create_dir(&staging_root)?;
    let result = apply_into_domain(plan, &domain_root, &staging_root);
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging_root);
        let _ = fs::remove_dir_all(&domain_root);
    }
    result
}

fn apply_into_domain(
    plan: &MigrationPlan,
    domain_root: &Path,
    staging_root: &Path,
) -> Result<MigrationReceipt> {
    let mut copied_roots = Vec::new();
    for source in &plan.sources {
        let kind = match source.kind {
            EntryKind::Skill => "skills",
            EntryKind::Command => "commands",
            EntryKind::Resource => bail!("resource roots cannot be migrated directly"),
        };
        let target = staging_root.join(kind).join(&source.name);
        copy_tree(&source.path, &target)?;
        copied_roots.push(SourceRoot {
            name: source.name.clone(),
            kind: source.kind,
            path: target,
        });
    }
    let catalog = build_catalog(&copied_roots, crate::DEFAULT_MAX_FILE_BYTES)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    doctor(&catalog).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let (catalog, catalog_path) = publish_staged_catalog(catalog, staging_root, domain_root)?;

    let registry_before = if plan.registry_path.exists() {
        Some(
            crate::load_domain_registry(&plan.registry_path)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?,
        )
    } else {
        None
    };
    let mut registry = registry_before.clone().unwrap_or_default();
    if registry.domains.contains_key(&plan.domain) {
        bail!("domain already exists in registry: {}", plan.domain);
    }
    registry.domains.insert(
        plan.domain.clone(),
        DomainConfig {
            catalog: catalog_path.clone(),
            usage_file: Some(domain_root.join("usage.json")),
            account: None,
        },
    );
    let adapter_path = plan
        .host_root
        .join("skills")
        .join("skillleaf")
        .join("SKILL.md");
    let adapter = adapter_body(&plan.domain, &plan.registry_path);
    let adapter_created = if adapter_path.exists() {
        if fs::read_to_string(&adapter_path)? != adapter {
            bail!(
                "host adapter already exists with different content: {}",
                adapter_path.display()
            );
        }
        false
    } else {
        if let Some(parent) = adapter_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&adapter_path, adapter.as_bytes())?;
        true
    };
    let adapter_sha256 = sha256(adapter.as_bytes());
    if let Err(error) = write_domain_registry_atomic(&registry, &plan.registry_path) {
        if adapter_created {
            let _ = fs::remove_file(&adapter_path);
        }
        return Err(anyhow::anyhow!(error.to_string()));
    }
    let registry_after_sha256 = sha256(&fs::read(&plan.registry_path)?);
    let domain_manifest_sha256 = directory_manifest_hash(domain_root)?;
    Ok(MigrationReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        domain: plan.domain.clone(),
        domain_root: domain_root.to_path_buf(),
        domain_manifest_sha256,
        catalog_sha256: catalog.catalog_sha256,
        registry_path: plan.registry_path.clone(),
        registry_before,
        registry_after_sha256,
        adapter_path,
        adapter_created,
        adapter_sha256,
    })
}

fn publish_staged_catalog(
    mut catalog: Catalog,
    staging_root: &Path,
    domain_root: &Path,
) -> Result<(Catalog, PathBuf)> {
    for root in catalog.roots.values_mut() {
        let relative = Path::new(root)
            .strip_prefix(staging_root)
            .with_context(|| format!("catalog root is outside staging: {root}"))?;
        *root = domain_root.join(relative).to_string_lossy().into_owned();
    }
    catalog.catalog_sha256 = crate::catalog_hash(&catalog.roots, &catalog.entries)?;
    write_catalog_atomic(&catalog, &staging_root.join("catalog.json"))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    fs::rename(staging_root, domain_root).with_context(|| {
        format!(
            "cannot atomically publish {} as {}",
            staging_root.display(),
            domain_root.display()
        )
    })?;
    let catalog_path = domain_root.join("catalog.json");
    Ok((catalog, catalog_path))
}

pub fn rollback_migration(receipt: &MigrationReceipt) -> SkillleafResult<()> {
    rollback_migration_inner(receipt).map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

fn rollback_migration_inner(receipt: &MigrationReceipt) -> Result<()> {
    if receipt.schema != RECEIPT_SCHEMA {
        bail!("unsupported migration receipt schema: {}", receipt.schema);
    }
    if sha256(&fs::read(&receipt.registry_path)?) != receipt.registry_after_sha256 {
        bail!("domain registry changed after migration; refusing rollback");
    }
    if directory_manifest_hash(&receipt.domain_root)? != receipt.domain_manifest_sha256 {
        bail!("migrated domain changed after apply; refusing rollback");
    }
    if receipt.adapter_created {
        if sha256(&fs::read(&receipt.adapter_path)?) != receipt.adapter_sha256 {
            bail!("host adapter changed after migration; refusing rollback");
        }
        fs::remove_file(&receipt.adapter_path)?;
    }
    if let Some(previous) = &receipt.registry_before {
        write_domain_registry_atomic(previous, &receipt.registry_path)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    } else {
        fs::remove_file(&receipt.registry_path)?;
    }
    fs::remove_dir_all(&receipt.domain_root)?;
    Ok(())
}

fn adapter_body(domain: &str, registry: &Path) -> String {
    format!(
        "---
name: skillleaf
description: Route local skills and commands lazily through Skill-Leaf.
---
# Skill-Leaf router

Resolve the current task with `skillleaf resolve --domain {domain} --registry \"{}\" --task \"<task>\" --limit 3`, then hydrate all selected selectors in one `skillleaf read --domain {domain} --registry \"{}\" --many <selectors>` call. Keep exactly one active router and do not preload unselected bodies. Native slash-command files remain owned by the host and must not be removed.
",
        registry.display(),
        registry.display()
    )
}

#[derive(Serialize)]
struct PlanMaterial<'a> {
    domain: &'a str,
    destination_root: &'a Path,
    registry_path: &'a Path,
    host: HostKind,
    host_root: &'a Path,
    sources: &'a [MigrationSource],
    preserves_native_commands: bool,
}

fn computed_plan_id(plan: &MigrationPlan) -> Result<String> {
    let material = PlanMaterial {
        domain: &plan.domain,
        destination_root: &plan.destination_root,
        registry_path: &plan.registry_path,
        host: plan.host,
        host_root: &plan.host_root,
        sources: &plan.sources,
        preserves_native_commands: plan.preserves_native_commands,
    };
    Ok(sha256(&serde_json::to_vec(&material)?)[..16].to_string())
}

fn validate_plan(plan: &MigrationPlan) -> Result<()> {
    if plan.schema != PLAN_SCHEMA || !plan.preserves_native_commands {
        bail!("invalid or unsafe migration plan");
    }
    validate_domain_name(&plan.domain)?;
    let expected = computed_plan_id(plan)?;
    if plan.plan_id != expected {
        bail!("migration plan hash mismatch; generate a new plan");
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for item in WalkDir::new(source).follow_links(false).sort_by_file_name() {
        let item = item?;
        let relative = item.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if item.file_type().is_symlink() {
            bail!(
                "migration refuses symlinked content: {}",
                item.path().display()
            );
        }
        if item.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if item.file_type().is_file() {
            if item.metadata()?.len() > MAX_MIGRATION_FILE_BYTES {
                bail!(
                    "migration file exceeds {} bytes: {}",
                    MAX_MIGRATION_FILE_BYTES,
                    item.path().display()
                );
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(item.path(), &target)?;
        }
    }
    Ok(())
}

fn directory_manifest_hash(root: &Path) -> Result<String> {
    let canonical = root.canonicalize()?;
    let mut manifest = BTreeMap::<String, String>::new();
    for item in WalkDir::new(&canonical)
        .follow_links(false)
        .sort_by_file_name()
    {
        let item = item?;
        if item.file_type().is_symlink() {
            bail!(
                "manifest refuses symlinked content: {}",
                item.path().display()
            );
        }
        if !item.file_type().is_file() {
            continue;
        }
        let relative = item
            .path()
            .strip_prefix(&canonical)?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        manifest.insert(relative, sha256(&fs::read(item.path())?));
    }
    Ok(sha256(&serde_json::to_vec(&manifest)?))
}

fn absolute_without_existing(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes its filesystem root: {}", path.display());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    let mut existing = normalized.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .with_context(|| format!("path has no existing ancestor: {}", path.display()))?;
        missing.push(name.to_os_string());
        existing = existing
            .parent()
            .with_context(|| format!("path has no existing ancestor: {}", path.display()))?;
    }
    let mut canonical = existing.canonicalize()?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}
