use anyhow::{Context, Result, bail};
use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;

mod usage;

pub use usage::{UsageEntry, UsageReport, record_hydrations, usage_report};

pub const CATALOG_SCHEMA: &str = "skillleaf.catalog.v1";
pub const DEFAULT_MAX_FILE_BYTES: u64 = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SkillleafError {
    #[error("catalog input error: {0}")]
    CatalogInput(String),
    #[error("catalog storage error: {0}")]
    Storage(String),
    #[error("resolution error: {0}")]
    Resolution(String),
    #[error("catalog integrity error: {0}")]
    Integrity(String),
}

pub type SkillleafResult<T> = std::result::Result<T, SkillleafError>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Skill,
    Command,
    Resource,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Resource => "resource",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Dependency {
    pub selector: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CatalogEntry {
    pub source: String,
    pub kind: EntryKind,
    pub name: String,
    pub description: String,
    pub relative_path: String,
    pub content_sha256: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<Dependency>,
}

impl CatalogEntry {
    pub fn selector(&self) -> String {
        format!("{}/{}:{}", self.source, self.kind.as_str(), self.name)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Catalog {
    pub schema: String,
    pub catalog_sha256: String,
    pub roots: BTreeMap<String, String>,
    pub entries: Vec<CatalogEntry>,
}

#[derive(Clone, Debug)]
pub struct SourceRoot {
    pub name: String,
    pub kind: EntryKind,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Resolution {
    pub schema: &'static str,
    pub catalog_sha256: String,
    pub task: String,
    pub selected: Vec<ResolvedEntry>,
    pub corpus_bytes: u64,
    pub selected_bytes: u64,
    pub avoided_bytes: u64,
    pub estimated_tokens_avoided: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResolvedEntry {
    pub selector: String,
    pub description: String,
    pub content_sha256: String,
    pub bytes: u64,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HydratedEntry {
    pub selector: String,
    pub content_sha256: String,
    pub content: String,
}

pub fn build_catalog(roots: &[SourceRoot], max_file_bytes: u64) -> SkillleafResult<Catalog> {
    build_catalog_inner(roots, max_file_bytes)
        .map_err(|error| SkillleafError::CatalogInput(format!("{error:#}")))
}

fn build_catalog_inner(roots: &[SourceRoot], max_file_bytes: u64) -> Result<Catalog> {
    if roots.is_empty() {
        bail!("at least one source root is required");
    }

    let mut root_map = BTreeMap::new();
    let mut entries = Vec::new();
    for root in roots {
        validate_source_name(&root.name)?;
        let canonical = root
            .path
            .canonicalize()
            .with_context(|| format!("cannot resolve source root {}", root.path.display()))?;
        if !canonical.is_dir() {
            bail!("source root is not a directory: {}", canonical.display());
        }
        let root_key = root_key(&root.name, root.kind);
        if root_map
            .insert(root_key.clone(), canonical.to_string_lossy().into_owned())
            .is_some()
        {
            bail!("duplicate source and kind: {root_key}");
        }
        match root.kind {
            EntryKind::Skill => index_skills(&root.name, &canonical, max_file_bytes, &mut entries)?,
            EntryKind::Command => {
                index_commands(&root.name, &canonical, max_file_bytes, &mut entries)?
            }
            EntryKind::Resource => bail!("resource roots are discovered through skill packages"),
        }
    }

    entries.sort_by_key(CatalogEntry::selector);
    let mut seen = BTreeSet::new();
    for entry in &entries {
        if !seen.insert(entry.selector()) {
            bail!("duplicate catalog selector: {}", entry.selector());
        }
    }
    validate_dependencies(&entries)?;

    let catalog_sha256 = catalog_hash(&root_map, &entries)?;
    Ok(Catalog {
        schema: CATALOG_SCHEMA.to_string(),
        catalog_sha256,
        roots: root_map,
        entries,
    })
}

pub fn write_catalog_atomic(catalog: &Catalog, output: &Path) -> SkillleafResult<()> {
    write_catalog_atomic_inner(catalog, output)
        .map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

fn write_catalog_atomic_inner(catalog: &Catalog, output: &Path) -> Result<()> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, catalog)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(output)
        .map_err(|error| error.error)
        .with_context(|| format!("cannot atomically replace {}", output.display()))?;
    Ok(())
}

pub fn load_catalog(path: &Path) -> SkillleafResult<Catalog> {
    load_catalog_inner(path).map_err(|error| SkillleafError::Integrity(format!("{error:#}")))
}

fn load_catalog_inner(path: &Path) -> Result<Catalog> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let catalog: Catalog = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid catalog JSON in {}", path.display()))?;
    if catalog.schema != CATALOG_SCHEMA {
        bail!("unsupported catalog schema: {}", catalog.schema);
    }
    let expected = catalog_hash(&catalog.roots, &catalog.entries)?;
    if expected != catalog.catalog_sha256 {
        bail!("catalog hash mismatch; rebuild the index before using it");
    }
    Ok(catalog)
}

pub fn resolve(
    catalog: &Catalog,
    task: &str,
    required: &[String],
    limit: usize,
) -> SkillleafResult<Resolution> {
    resolve_inner(catalog, task, required, limit)
        .map_err(|error| SkillleafError::Resolution(format!("{error:#}")))
}

fn resolve_inner(
    catalog: &Catalog,
    task: &str,
    required: &[String],
    limit: usize,
) -> Result<Resolution> {
    let task_tokens = tokens(task);
    let index = catalog
        .entries
        .iter()
        .map(|entry| (entry.selector(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut selected = BTreeMap::<String, (u32, BTreeSet<String>)>::new();

    for entry in &catalog.entries {
        if entry.kind == EntryKind::Resource {
            continue;
        }
        let score = match_score(entry, &task_tokens);
        if score > 0 {
            selected.entry(entry.selector()).or_default().0 = score;
            selected
                .entry(entry.selector())
                .or_default()
                .1
                .insert("task match".to_string());
        }
    }
    for selector in required {
        if !index.contains_key(selector) {
            bail!("required selector is not in the catalog: {selector}");
        }
        let candidate = selected.entry(selector.clone()).or_default();
        candidate.0 = u32::MAX;
        candidate.1.insert("explicit request".to_string());
    }

    let mut ranked = selected.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.0.cmp(&left.1.0).then_with(|| left.0.cmp(&right.0)));
    ranked.truncate(limit.max(required.len()));
    let mut closure = ranked.into_iter().collect::<BTreeMap<_, _>>();
    close_dependencies(&index, &mut closure)?;

    let corpus_bytes = catalog.entries.iter().map(|entry| entry.bytes).sum::<u64>();
    let selected_bytes = closure
        .keys()
        .filter_map(|selector| index.get(selector))
        .map(|entry| entry.bytes)
        .sum::<u64>();
    let avoided_bytes = corpus_bytes.saturating_sub(selected_bytes);
    let selected = closure
        .into_iter()
        .filter_map(|(selector, (_, reasons))| {
            let entry = index.get(&selector)?;
            Some(ResolvedEntry {
                selector,
                description: entry.description.clone(),
                content_sha256: entry.content_sha256.clone(),
                bytes: entry.bytes,
                reasons: reasons.into_iter().collect(),
            })
        })
        .collect();

    Ok(Resolution {
        schema: "skillleaf.resolution.v1",
        catalog_sha256: catalog.catalog_sha256.clone(),
        task: task.to_string(),
        selected,
        corpus_bytes,
        selected_bytes,
        avoided_bytes,
        estimated_tokens_avoided: avoided_bytes / 4,
    })
}

pub fn hydrate(catalog: &Catalog, selectors: &[String]) -> SkillleafResult<Vec<HydratedEntry>> {
    hydrate_inner(catalog, selectors)
        .map_err(|error| SkillleafError::Integrity(format!("{error:#}")))
}

fn hydrate_inner(catalog: &Catalog, selectors: &[String]) -> Result<Vec<HydratedEntry>> {
    let index = catalog
        .entries
        .iter()
        .map(|entry| (entry.selector(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut hydrated = Vec::new();
    let mut unique = BTreeSet::new();
    for selector in selectors {
        if !unique.insert(selector) {
            continue;
        }
        let entry = index
            .get(selector)
            .with_context(|| format!("selector is not in the catalog: {selector}"))?;
        let root = catalog
            .roots
            .get(&root_key(&entry.source, entry.kind))
            .with_context(|| format!("missing root for selector {selector}"))?;
        let path = contained_path(Path::new(root), &entry.relative_path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("catalog body is not a regular file: {}", path.display());
        }
        let bytes = fs::read(&path)?;
        let actual_hash = sha256(&bytes);
        if actual_hash != entry.content_sha256 {
            bail!("content hash mismatch for {selector}; rebuild the catalog");
        }
        let content = String::from_utf8(bytes)
            .with_context(|| format!("catalog body is not UTF-8: {}", path.display()))?;
        hydrated.push(HydratedEntry {
            selector: selector.clone(),
            content_sha256: actual_hash,
            content,
        });
    }
    Ok(hydrated)
}

pub fn doctor(catalog: &Catalog) -> SkillleafResult<()> {
    doctor_inner(catalog).map_err(|error| SkillleafError::Integrity(format!("{error:#}")))
}

fn doctor_inner(catalog: &Catalog) -> Result<()> {
    let selectors = catalog
        .entries
        .iter()
        .map(CatalogEntry::selector)
        .collect::<Vec<_>>();
    hydrate_inner(catalog, &selectors)?;
    validate_dependencies(&catalog.entries)
}

fn index_skills(
    source: &str,
    root: &Path,
    max_file_bytes: u64,
    entries: &mut Vec<CatalogEntry>,
) -> Result<()> {
    for item in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let item = item?;
        if item.file_type().is_symlink() || !item.file_type().is_file() {
            continue;
        }
        if item.file_name() != "SKILL.md" {
            continue;
        }
        let skill_path = item.path();
        let package_root = skill_path.parent().context("SKILL.md has no parent")?;
        let body = read_bounded_utf8(skill_path, max_file_bytes)?;
        let frontmatter = parse_frontmatter(&body);
        let default_name = package_root
            .strip_prefix(root)?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let name = frontmatter.name.unwrap_or(default_name);
        validate_entry_name(&name)?;
        let mut dependencies = frontmatter
            .dependencies
            .into_iter()
            .map(|selector| Dependency { selector })
            .collect::<Vec<_>>();
        dependencies.extend(linked_resources(source, root, package_root, &name, &body)?);
        dependencies.sort_by(|left, right| left.selector.cmp(&right.selector));
        dependencies.dedup();
        entries.push(make_entry(
            source,
            EntryKind::Skill,
            name.clone(),
            frontmatter
                .description
                .unwrap_or_else(|| first_heading(&body)),
            root,
            skill_path,
            dependencies,
        )?);

        for resource in WalkDir::new(package_root)
            .follow_links(false)
            .sort_by_file_name()
        {
            let resource = resource?;
            if resource.file_type().is_symlink()
                || !resource.file_type().is_file()
                || resource.path() == skill_path
                || resource.path().extension().and_then(|value| value.to_str()) != Some("md")
            {
                continue;
            }
            let _ = read_bounded_utf8(resource.path(), max_file_bytes)?;
            let relative = resource.path().strip_prefix(package_root)?;
            let resource_name = format!(
                "{}/{}",
                name,
                relative
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            );
            entries.push(make_entry(
                source,
                EntryKind::Resource,
                resource_name,
                format!("Resource for skill {name}"),
                root,
                resource.path(),
                Vec::new(),
            )?);
        }
    }
    Ok(())
}

fn index_commands(
    source: &str,
    root: &Path,
    max_file_bytes: u64,
    entries: &mut Vec<CatalogEntry>,
) -> Result<()> {
    for item in WalkDir::new(root).follow_links(false).sort_by_file_name() {
        let item = item?;
        if item.file_type().is_symlink()
            || !item.file_type().is_file()
            || item.path().extension().and_then(|value| value.to_str()) != Some("md")
        {
            continue;
        }
        let body = read_bounded_utf8(item.path(), max_file_bytes)?;
        let frontmatter = parse_frontmatter(&body);
        let default_name = item
            .path()
            .strip_prefix(root)?
            .with_extension("")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let name = frontmatter.name.unwrap_or(default_name);
        validate_entry_name(&name)?;
        let dependencies = frontmatter
            .dependencies
            .into_iter()
            .map(|selector| Dependency { selector })
            .collect();
        entries.push(make_entry(
            source,
            EntryKind::Command,
            name,
            frontmatter
                .description
                .unwrap_or_else(|| first_heading(&body)),
            root,
            item.path(),
            dependencies,
        )?);
    }
    Ok(())
}

fn make_entry(
    source: &str,
    kind: EntryKind,
    name: String,
    description: String,
    root: &Path,
    path: &Path,
    dependencies: Vec<Dependency>,
) -> Result<CatalogEntry> {
    let bytes = fs::read(path)?;
    Ok(CatalogEntry {
        source: source.to_string(),
        kind,
        name,
        description: compact_description(&description),
        relative_path: path
            .strip_prefix(root)?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
        content_sha256: sha256(&bytes),
        bytes: bytes.len() as u64,
        dependencies,
    })
}

fn close_dependencies(
    index: &BTreeMap<String, &CatalogEntry>,
    selected: &mut BTreeMap<String, (u32, BTreeSet<String>)>,
) -> Result<()> {
    let mut pending = selected.keys().cloned().collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    while let Some(selector) = pending.pop() {
        if !visited.insert(selector.clone()) {
            continue;
        }
        let entry = index
            .get(&selector)
            .with_context(|| format!("selected entry disappeared: {selector}"))?;
        for dependency in &entry.dependencies {
            if !index.contains_key(&dependency.selector) {
                bail!(
                    "missing dependency {} required by {}",
                    dependency.selector,
                    selector
                );
            }
            let candidate = selected.entry(dependency.selector.clone()).or_default();
            candidate.1.insert(format!("required by {selector}"));
            pending.push(dependency.selector.clone());
        }
    }
    Ok(())
}

fn linked_resources(
    source: &str,
    source_root: &Path,
    package_root: &Path,
    skill_name: &str,
    body: &str,
) -> Result<Vec<Dependency>> {
    let mut dependencies = BTreeSet::new();
    for event in Parser::new_ext(body, Options::all()) {
        let Event::Start(Tag::Link { dest_url, .. }) = event else {
            continue;
        };
        let destination = dest_url.as_ref();
        if destination.contains("://") || destination.starts_with('#') {
            continue;
        }
        let path_only = destination.split('#').next().unwrap_or(destination);
        if Path::new(path_only)
            .extension()
            .and_then(|value| value.to_str())
            != Some("md")
        {
            continue;
        }
        let linked = contained_path(package_root, path_only)?;
        if !linked.starts_with(source_root) || !linked.is_file() {
            bail!("linked skill resource is unavailable: {destination}");
        }
        let relative = linked.strip_prefix(package_root)?;
        dependencies.insert(format!(
            "{source}/resource:{skill_name}/{}",
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        ));
    }
    Ok(dependencies
        .into_iter()
        .map(|selector| Dependency { selector })
        .collect())
}

fn parse_frontmatter(body: &str) -> Frontmatter {
    let mut lines = body.lines();
    if lines.next() != Some("---") {
        return Frontmatter::default();
    }
    let yaml = lines
        .by_ref()
        .take_while(|line| *line != "---")
        .collect::<Vec<_>>()
        .join("\n");
    parse_frontmatter_fields(&yaml)
}

fn parse_frontmatter_fields(yaml: &str) -> Frontmatter {
    let mut frontmatter = Frontmatter::default();
    let mut reading_dependencies = false;
    for line in yaml.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("name:") {
            frontmatter.name = nonempty_scalar(value);
            reading_dependencies = false;
        } else if let Some(value) = trimmed.strip_prefix("description:") {
            frontmatter.description = nonempty_scalar(value);
            reading_dependencies = false;
        } else if trimmed == "dependencies:" {
            reading_dependencies = true;
        } else if reading_dependencies {
            if let Some(value) = trimmed.strip_prefix('-') {
                if let Some(value) = nonempty_scalar(value) {
                    frontmatter.dependencies.push(value);
                }
            } else if !trimmed.is_empty() {
                reading_dependencies = false;
            }
        }
    }
    frontmatter
}

fn nonempty_scalar(value: &str) -> Option<String> {
    let value = value.trim().trim_matches(['\'', '"']);
    (!value.is_empty()).then(|| value.to_string())
}

fn first_heading(body: &str) -> String {
    body.lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|value| !value.is_empty())
        .unwrap_or("Agent capability")
        .to_string()
}

fn compact_description(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(240).collect()
}

fn tokens(value: &str) -> BTreeSet<String> {
    const STOP: &[&str] = &[
        "about", "agent", "and", "for", "from", "into", "skill", "skills", "the", "this", "use",
        "when", "with",
    ];
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|token| token.len() >= 3 && !STOP.contains(token))
        .map(ToOwned::to_owned)
        .collect()
}

fn match_score(entry: &CatalogEntry, task_tokens: &BTreeSet<String>) -> u32 {
    let name_tokens = tokens(&entry.name);
    let description_tokens = tokens(&entry.description);
    let name_matches = task_tokens
        .iter()
        .filter(|token| name_tokens.contains(*token))
        .count() as u32;
    let description_matches = task_tokens
        .iter()
        .filter(|token| description_tokens.contains(*token))
        .count() as u32;
    if name_matches == 0 && description_matches < 2 {
        0
    } else {
        name_matches * 12 + description_matches * 2
    }
}

fn validate_dependencies(entries: &[CatalogEntry]) -> Result<()> {
    let selectors = entries
        .iter()
        .map(CatalogEntry::selector)
        .collect::<BTreeSet<_>>();
    for entry in entries {
        for dependency in &entry.dependencies {
            if !selectors.contains(&dependency.selector) {
                bail!(
                    "missing dependency {} required by {}",
                    dependency.selector,
                    entry.selector()
                );
            }
        }
    }
    Ok(())
}

fn validate_source_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
    {
        bail!("source names must contain only letters, numbers, '-' or '_': {name}");
    }
    Ok(())
}

fn root_key(source: &str, kind: EntryKind) -> String {
    let root_kind = if kind == EntryKind::Resource {
        EntryKind::Skill
    } else {
        kind
    };
    format!("{source}/{}", root_kind.as_str())
}

fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
        bail!("unsafe or empty catalog entry name: {name}");
    }
    Ok(())
}

fn read_bounded_utf8(path: &Path, max_file_bytes: u64) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("catalog inputs must be regular files: {}", path.display());
    }
    if metadata.len() > max_file_bytes {
        bail!(
            "catalog input exceeds {} bytes: {}",
            max_file_bytes,
            path.display()
        );
    }
    fs::read_to_string(path)
        .with_context(|| format!("catalog input is not UTF-8: {}", path.display()))
}

fn contained_path(root: &Path, relative: impl AsRef<Path>) -> Result<PathBuf> {
    let relative = relative.as_ref();
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!(
            "catalog path escapes its source root: {}",
            relative.display()
        );
    }
    let root = root.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(&root) {
        bail!("catalog path escapes its source root: {}", path.display());
    }
    Ok(path)
}

fn catalog_hash(roots: &BTreeMap<String, String>, entries: &[CatalogEntry]) -> Result<String> {
    #[derive(Serialize)]
    struct Material<'a> {
        schema: &'a str,
        roots: &'a BTreeMap<String, String>,
        entries: &'a [CatalogEntry],
    }
    Ok(sha256(&serde_json::to_vec(&Material {
        schema: CATALOG_SCHEMA,
        roots,
        entries,
    })?))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
