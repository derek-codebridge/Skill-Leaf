use crate::{SkillleafError, SkillleafResult, write_json_atomic};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const DOMAIN_REGISTRY_SCHEMA: &str = "skillleaf.domain-registry.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainConfig {
    pub catalog: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DomainRegistry {
    pub schema: String,
    pub domains: BTreeMap<String, DomainConfig>,
}

impl Default for DomainRegistry {
    fn default() -> Self {
        Self {
            schema: DOMAIN_REGISTRY_SCHEMA.to_string(),
            domains: BTreeMap::new(),
        }
    }
}

pub fn load_domain_registry(path: &Path) -> SkillleafResult<DomainRegistry> {
    load_registry(path).map_err(|error| SkillleafError::Integrity(format!("{error:#}")))
}

fn load_registry(path: &Path) -> Result<DomainRegistry> {
    if !path.exists() {
        return Ok(DomainRegistry::default());
    }
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let registry: DomainRegistry = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid domain registry JSON in {}", path.display()))?;
    if registry.schema != DOMAIN_REGISTRY_SCHEMA {
        bail!("unsupported domain registry schema: {}", registry.schema);
    }
    for name in registry.domains.keys() {
        validate_domain_name(name)?;
    }
    Ok(registry)
}

pub fn write_domain_registry_atomic(registry: &DomainRegistry, path: &Path) -> SkillleafResult<()> {
    for name in registry.domains.keys() {
        validate_domain_name(name)
            .map_err(|error| SkillleafError::Storage(format!("{error:#}")))?;
    }
    write_json_atomic(registry, path).map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

pub fn domain_catalog_path(
    explicit_catalog: Option<PathBuf>,
    domain: Option<&str>,
    registry_path: &Path,
) -> SkillleafResult<PathBuf> {
    if explicit_catalog.is_some() && domain.is_some() {
        return Err(SkillleafError::CatalogInput(
            "use either --catalog or --domain, not both".to_string(),
        ));
    }
    if let Some(catalog) = explicit_catalog {
        return Ok(catalog);
    }
    let Some(domain) = domain else {
        return Ok(PathBuf::from("skillleaf.json"));
    };
    validate_domain_name(domain)
        .map_err(|error| SkillleafError::CatalogInput(format!("{error:#}")))?;
    let registry = load_domain_registry(registry_path)?;
    registry
        .domains
        .get(domain)
        .map(|config| config.catalog.clone())
        .ok_or_else(|| SkillleafError::CatalogInput(format!("unknown Skill-Leaf domain: {domain}")))
}

pub(crate) fn validate_domain_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        bail!("domain names must contain only letters, numbers, '-' or '_': {name}");
    }
    Ok(())
}
