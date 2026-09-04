use crate::domain::{DomainConfig, validate_domain_name};
use crate::{
    Catalog, SkillleafError, SkillleafResult, TrustLevel, catalog_hash, contained_path, doctor,
    load_catalog, load_domain_registry, root_key, sha256, write_catalog_atomic,
    write_domain_registry_atomic, write_json_atomic,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const SYNC_PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_SYNC_CHUNK_BYTES: usize = 256 * 1024;
const MAX_SYNC_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_SYNC_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SYNC_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SYNC_FILES: usize = 10_000;
const MANIFEST_SCHEMA: &str = "skillleaf.sync-manifest.v1";
const POINTER_SCHEMA: &str = "skillleaf.sync-pointer.v1";
const RECEIPT_SCHEMA: &str = "skillleaf.sync-receipt.v1";
const STATUS_SCHEMA: &str = "skillleaf.sync-status.v1";
const LOCAL_STATE_SCHEMA: &str = "skillleaf.sync-local-state.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncChunk {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SyncFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub chunks: Vec<SyncChunk>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyncManifest {
    pub schema: String,
    pub protocol_min: u32,
    pub protocol_max: u32,
    pub snapshot_id: String,
    pub catalog: Catalog,
    pub files: Vec<SyncFile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SyncPointer {
    schema: String,
    protocol_min: u32,
    protocol_max: u32,
    snapshot_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LocalState {
    schema: String,
    protocol: u32,
    snapshot_id: String,
    catalog_path: PathBuf,
    trusted: bool,
}

#[derive(Clone, Debug, Default)]
pub struct PullOptions {
    pub expected_snapshot: Option<String>,
    pub trust_remote: bool,
    pub allow_offline_fallback: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncReceipt {
    pub schema: &'static str,
    pub mode: &'static str,
    pub snapshot_id: String,
    pub domain: String,
    pub catalog_path: PathBuf,
    pub trusted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncStatus {
    pub schema: &'static str,
    pub remote_available: bool,
    pub remote_snapshot: Option<String>,
    pub local_snapshot: Option<String>,
    pub local_verified: bool,
    pub update_available: bool,
    pub fallback_ready: bool,
}

pub fn publish_snapshot(
    catalog_path: &Path,
    remote: &Path,
    chunk_bytes: usize,
) -> SkillleafResult<SyncManifest> {
    publish_snapshot_inner(catalog_path, remote, chunk_bytes)
        .map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

fn publish_snapshot_inner(
    catalog_path: &Path,
    remote: &Path,
    chunk_bytes: usize,
) -> Result<SyncManifest> {
    if !(1..=MAX_SYNC_CHUNK_BYTES).contains(&chunk_bytes) {
        bail!("sync chunk size must be between 1 and {MAX_SYNC_CHUNK_BYTES} bytes");
    }
    let mut catalog =
        load_catalog(catalog_path).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    doctor(&catalog).map_err(|error| anyhow::anyhow!(error.to_string()))?;

    let original_roots = catalog.roots.clone();
    for (key, root) in &mut catalog.roots {
        validate_root_key(key)?;
        *root = format!("roots/{key}");
    }
    catalog.catalog_sha256 = catalog_hash(&catalog.roots, &catalog.entries)?;

    let mut bodies = BTreeMap::<String, Vec<u8>>::new();
    for entry in &catalog.entries {
        let key = root_key(&entry.source, entry.kind);
        let source_root = original_roots
            .get(&key)
            .with_context(|| format!("catalogue entry has no source root: {}", entry.selector()))?;
        let source = contained_path(Path::new(source_root), &entry.relative_path)?;
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("sync input is not a regular file: {}", source.display());
        }
        if metadata.len() > MAX_SYNC_FILE_BYTES {
            bail!(
                "sync input exceeds {MAX_SYNC_FILE_BYTES} bytes: {}",
                source.display()
            );
        }
        let bytes = fs::read(&source)?;
        if sha256(&bytes) != entry.content_sha256 {
            bail!("catalogue body changed before sync: {}", entry.selector());
        }
        let relative = format!("roots/{key}/{}", entry.relative_path);
        portable_path(&relative)?;
        if let Some(previous) = bodies.insert(relative.clone(), bytes.clone())
            && previous != bytes
        {
            bail!("two catalogue entries map to different content at {relative}");
        }
    }
    if bodies.len() > MAX_SYNC_FILES {
        bail!("sync snapshot exceeds {MAX_SYNC_FILES} files");
    }
    let total_bytes = bodies.values().map(|body| body.len() as u64).sum::<u64>();
    if total_bytes > MAX_SYNC_TOTAL_BYTES {
        bail!("sync snapshot exceeds {MAX_SYNC_TOTAL_BYTES} bytes");
    }

    let files = chunk_bodies(remote, bodies, chunk_bytes)?;

    let mut manifest = SyncManifest {
        schema: MANIFEST_SCHEMA.to_string(),
        protocol_min: SYNC_PROTOCOL_VERSION,
        protocol_max: SYNC_PROTOCOL_VERSION,
        snapshot_id: String::new(),
        catalog,
        files,
    };
    manifest.snapshot_id = manifest_hash(&manifest)?;
    fs::create_dir_all(remote.join("snapshots"))?;
    write_json_atomic(
        &manifest,
        &remote
            .join("snapshots")
            .join(format!("{}.json", manifest.snapshot_id)),
    )?;
    write_json_atomic(
        &SyncPointer {
            schema: POINTER_SCHEMA.to_string(),
            protocol_min: SYNC_PROTOCOL_VERSION,
            protocol_max: SYNC_PROTOCOL_VERSION,
            snapshot_id: manifest.snapshot_id.clone(),
        },
        &remote.join("current.json"),
    )?;
    Ok(manifest)
}

fn chunk_bodies(
    remote: &Path,
    bodies: BTreeMap<String, Vec<u8>>,
    chunk_bytes: usize,
) -> Result<Vec<SyncFile>> {
    let mut files = Vec::with_capacity(bodies.len());
    for (path, bytes) in bodies {
        let mut chunks = Vec::new();
        for chunk in bytes.chunks(chunk_bytes) {
            let digest = sha256(chunk);
            write_chunk(remote, &digest, chunk)?;
            chunks.push(SyncChunk {
                sha256: digest,
                bytes: chunk.len() as u64,
            });
        }
        files.push(SyncFile {
            path,
            sha256: sha256(&bytes),
            bytes: bytes.len() as u64,
            chunks,
        });
    }
    Ok(files)
}

pub fn pull_snapshot(
    remote: &Path,
    destination: &Path,
    registry_path: &Path,
    domain: &str,
    options: &PullOptions,
) -> SkillleafResult<SyncReceipt> {
    pull_snapshot_inner(remote, destination, registry_path, domain, options)
        .map_err(|error| SkillleafError::Storage(format!("{error:#}")))
}

fn pull_snapshot_inner(
    remote: &Path,
    destination: &Path,
    registry_path: &Path,
    domain: &str,
    options: &PullOptions,
) -> Result<SyncReceipt> {
    validate_domain_name(domain)?;
    if options.expected_snapshot.is_some() && options.trust_remote {
        bail!("use either an expected snapshot pin or --trust-remote, not both");
    }
    let pointer_bytes = match fs::read(remote.join("current.json")) {
        Ok(bytes) => bytes,
        Err(error) if options.allow_offline_fallback => {
            return offline_fallback(destination, registry_path, domain)
                .with_context(|| format!("remote unavailable ({error}); offline fallback failed"));
        }
        Err(error) => return Err(error).context("cannot read remote sync pointer"),
    };
    let pointer: SyncPointer =
        serde_json::from_slice(&pointer_bytes).context("invalid remote sync pointer")?;
    validate_pointer(&pointer)?;
    let manifest_path = remote
        .join("snapshots")
        .join(format!("{}.json", pointer.snapshot_id));
    let manifest: SyncManifest = serde_json::from_slice(
        &fs::read(&manifest_path)
            .with_context(|| format!("cannot read remote snapshot {}", manifest_path.display()))?,
    )
    .context("invalid remote sync manifest")?;
    validate_manifest(&manifest)?;
    if manifest.snapshot_id != pointer.snapshot_id {
        bail!("remote pointer and manifest snapshot identifiers differ");
    }
    if let Some(expected) = options.expected_snapshot.as_deref()
        && expected != manifest.snapshot_id
    {
        bail!(
            "remote snapshot does not match the pinned snapshot: expected {expected}, found {}",
            manifest.snapshot_id
        );
    }
    let trusted = options.trust_remote || options.expected_snapshot.is_some();
    materialize_snapshot(
        remote,
        destination,
        registry_path,
        domain,
        manifest,
        trusted,
    )
}

fn materialize_snapshot(
    remote: &Path,
    destination: &Path,
    registry_path: &Path,
    domain: &str,
    manifest: SyncManifest,
    trusted: bool,
) -> Result<SyncReceipt> {
    let domain_root = destination.join("domains").join(domain);
    let snapshots_root = domain_root.join("snapshots");
    fs::create_dir_all(&snapshots_root)?;
    let trust_suffix = if trusted { "trusted" } else { "untrusted" };
    let generation = snapshots_root.join(format!("{}-{trust_suffix}", manifest.snapshot_id));
    let catalog_path = generation.join("catalog.json");

    if !generation.exists() {
        let staging = tempfile::Builder::new()
            .prefix(".skillleaf-sync-")
            .tempdir_in(&snapshots_root)?;
        for file in &manifest.files {
            let relative = portable_path(&file.path)?;
            let target = staging.path().join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)?;
            let mut written = Vec::with_capacity(file.bytes as usize);
            for chunk in &file.chunks {
                let chunk_path = chunk_path(remote, &chunk.sha256);
                let metadata = fs::symlink_metadata(&chunk_path)
                    .with_context(|| format!("missing sync chunk {}", chunk.sha256))?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    bail!("sync chunk is not a regular file: {}", chunk.sha256);
                }
                let bytes = fs::read(&chunk_path)?;
                if bytes.len() as u64 != chunk.bytes || sha256(&bytes) != chunk.sha256 {
                    bail!("sync chunk failed verification: {}", chunk.sha256);
                }
                written.extend_from_slice(&bytes);
                output.write_all(&bytes)?;
            }
            output.sync_all()?;
            if written.len() as u64 != file.bytes || sha256(&written) != file.sha256 {
                bail!("materialized file failed verification: {}", file.path);
            }
        }

        let mut catalog = manifest.catalog.clone();
        if !trusted {
            for entry in &mut catalog.entries {
                entry.trust = TrustLevel::Untrusted;
            }
        }
        rebase_catalog(&mut catalog, staging.path())?;
        write_catalog_atomic(&catalog, &staging.path().join("catalog.json"))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        doctor(&catalog).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        catalog = manifest.catalog.clone();
        if !trusted {
            for entry in &mut catalog.entries {
                entry.trust = TrustLevel::Untrusted;
            }
        }
        rebase_catalog(&mut catalog, &generation)?;
        write_catalog_atomic(&catalog, &staging.path().join("catalog.json"))
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        fs::rename(staging.path(), &generation).with_context(|| {
            format!(
                "cannot atomically publish local snapshot {}",
                generation.display()
            )
        })?;
    }

    let catalog =
        load_catalog(&catalog_path).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    doctor(&catalog).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    bind_domain(registry_path, domain, &domain_root, &catalog_path)?;
    write_json_atomic(
        &LocalState {
            schema: LOCAL_STATE_SCHEMA.to_string(),
            protocol: SYNC_PROTOCOL_VERSION,
            snapshot_id: manifest.snapshot_id.clone(),
            catalog_path: catalog_path.clone(),
            trusted,
        },
        &domain_root.join("current.json"),
    )?;
    Ok(SyncReceipt {
        schema: RECEIPT_SCHEMA,
        mode: "remote",
        snapshot_id: manifest.snapshot_id,
        domain: domain.to_string(),
        catalog_path,
        trusted,
    })
}

fn offline_fallback(destination: &Path, registry_path: &Path, domain: &str) -> Result<SyncReceipt> {
    let domain_root = destination.join("domains").join(domain);
    let state_path = domain_root.join("current.json");
    let state: LocalState = serde_json::from_slice(
        &fs::read(&state_path)
            .with_context(|| format!("cannot read local sync state {}", state_path.display()))?,
    )
    .context("invalid local sync state")?;
    if state.schema != LOCAL_STATE_SCHEMA || state.protocol != SYNC_PROTOCOL_VERSION {
        bail!("unsupported local sync state");
    }
    let catalog =
        load_catalog(&state.catalog_path).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    doctor(&catalog).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    bind_domain(registry_path, domain, &domain_root, &state.catalog_path)?;
    Ok(SyncReceipt {
        schema: RECEIPT_SCHEMA,
        mode: "offline_fallback",
        snapshot_id: state.snapshot_id,
        domain: domain.to_string(),
        catalog_path: state.catalog_path,
        trusted: state.trusted,
    })
}

pub fn sync_status(remote: &Path, destination: &Path, domain: &str) -> SkillleafResult<SyncStatus> {
    sync_status_inner(remote, destination, domain)
        .map_err(|error| SkillleafError::Integrity(format!("{error:#}")))
}

fn sync_status_inner(remote: &Path, destination: &Path, domain: &str) -> Result<SyncStatus> {
    validate_domain_name(domain)?;
    let remote_state = match fs::read(remote.join("current.json")) {
        Ok(bytes) => {
            let pointer: SyncPointer =
                serde_json::from_slice(&bytes).context("invalid remote sync pointer")?;
            validate_pointer(&pointer)?;
            Some(pointer.snapshot_id)
        }
        Err(_) => None,
    };
    let local_path = destination
        .join("domains")
        .join(domain)
        .join("current.json");
    let (local_snapshot, local_verified) = match fs::read(&local_path) {
        Ok(bytes) => {
            let state: LocalState =
                serde_json::from_slice(&bytes).context("invalid local sync state")?;
            if state.schema != LOCAL_STATE_SCHEMA || state.protocol != SYNC_PROTOCOL_VERSION {
                bail!("unsupported local sync state");
            }
            let verified = load_catalog(&state.catalog_path)
                .and_then(|catalog| doctor(&catalog))
                .is_ok();
            (Some(state.snapshot_id), verified)
        }
        Err(_) => (None, false),
    };
    let update_available = remote_state.is_some() && remote_state != local_snapshot;
    Ok(SyncStatus {
        schema: STATUS_SCHEMA,
        remote_available: remote_state.is_some(),
        remote_snapshot: remote_state,
        local_snapshot,
        local_verified,
        update_available,
        fallback_ready: local_verified,
    })
}

fn bind_domain(
    registry_path: &Path,
    domain: &str,
    domain_root: &Path,
    catalog_path: &Path,
) -> Result<()> {
    let mut registry =
        load_domain_registry(registry_path).map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let previous = registry.domains.get(domain).cloned();
    registry.domains.insert(
        domain.to_string(),
        DomainConfig {
            catalog: catalog_path.to_path_buf(),
            usage_file: previous
                .as_ref()
                .and_then(|value| value.usage_file.clone())
                .or_else(|| Some(domain_root.join("usage.json"))),
            account: previous.and_then(|value| value.account),
        },
    );
    write_domain_registry_atomic(&registry, registry_path)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn validate_pointer(pointer: &SyncPointer) -> Result<()> {
    if pointer.schema != POINTER_SCHEMA {
        bail!("unsupported sync pointer schema: {}", pointer.schema);
    }
    negotiate_protocol(pointer.protocol_min, pointer.protocol_max)?;
    validate_snapshot_id(&pointer.snapshot_id)
}

fn validate_manifest(manifest: &SyncManifest) -> Result<()> {
    if manifest.schema != MANIFEST_SCHEMA {
        bail!("unsupported sync manifest schema: {}", manifest.schema);
    }
    negotiate_protocol(manifest.protocol_min, manifest.protocol_max)?;
    validate_snapshot_id(&manifest.snapshot_id)?;
    if manifest_hash(manifest)? != manifest.snapshot_id {
        bail!("sync manifest hash mismatch");
    }
    if manifest.files.len() > MAX_SYNC_FILES {
        bail!("sync manifest exceeds {MAX_SYNC_FILES} files");
    }
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    for file in &manifest.files {
        portable_path(&file.path)?;
        if !paths.insert(&file.path) {
            bail!("duplicate sync file path: {}", file.path);
        }
        if file.bytes > MAX_SYNC_FILE_BYTES {
            bail!(
                "sync file exceeds {MAX_SYNC_FILE_BYTES} bytes: {}",
                file.path
            );
        }
        let declared = file.chunks.iter().map(|chunk| chunk.bytes).sum::<u64>();
        if declared != file.bytes {
            bail!("sync chunk lengths do not match file length: {}", file.path);
        }
        for chunk in &file.chunks {
            validate_snapshot_id(&chunk.sha256)?;
            if chunk.bytes == 0 || chunk.bytes > MAX_SYNC_CHUNK_BYTES as u64 {
                bail!("invalid sync chunk size for {}", file.path);
            }
        }
        total = total
            .checked_add(file.bytes)
            .context("sync manifest byte count overflow")?;
    }
    if total > MAX_SYNC_TOTAL_BYTES {
        bail!("sync manifest exceeds {MAX_SYNC_TOTAL_BYTES} bytes");
    }
    Ok(())
}

fn negotiate_protocol(minimum: u32, maximum: u32) -> Result<u32> {
    if minimum > maximum {
        bail!("invalid sync protocol range {minimum}..={maximum}");
    }
    if minimum > SYNC_PROTOCOL_VERSION || maximum < SYNC_PROTOCOL_VERSION {
        bail!(
            "incompatible sync protocol {minimum}..={maximum}; this client supports {SYNC_PROTOCOL_VERSION}"
        );
    }
    Ok(SYNC_PROTOCOL_VERSION)
}

fn manifest_hash(manifest: &SyncManifest) -> Result<String> {
    #[derive(Serialize)]
    struct HashInput<'a> {
        schema: &'a str,
        protocol_min: u32,
        protocol_max: u32,
        catalog: &'a Catalog,
        files: &'a [SyncFile],
    }
    Ok(sha256(&serde_json::to_vec(&HashInput {
        schema: &manifest.schema,
        protocol_min: manifest.protocol_min,
        protocol_max: manifest.protocol_max,
        catalog: &manifest.catalog,
        files: &manifest.files,
    })?))
}

fn rebase_catalog(catalog: &mut Catalog, root: &Path) -> Result<()> {
    for relative in catalog.roots.values_mut() {
        let safe = portable_path(relative)?;
        *relative = root.join(safe).to_string_lossy().into_owned();
    }
    catalog.catalog_sha256 = catalog_hash(&catalog.roots, &catalog.entries)?;
    Ok(())
}

fn write_chunk(remote: &Path, digest: &str, bytes: &[u8]) -> Result<()> {
    let path = chunk_path(remote, digest);
    if path.exists() {
        let existing = fs::read(&path)?;
        if sha256(&existing) != digest {
            bail!("existing content-addressed chunk is corrupt: {digest}");
        }
        return Ok(());
    }
    let parent = path.parent().context("chunk path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist(&path) {
        Ok(_) => Ok(()),
        Err(error) => {
            if path.exists() && sha256(&fs::read(&path)?) == digest {
                Ok(())
            } else {
                Err(error.error).with_context(|| format!("cannot publish chunk {digest}"))
            }
        }
    }
}

fn chunk_path(remote: &Path, digest: &str) -> PathBuf {
    remote
        .join("chunks")
        .join(&digest[..2])
        .join(format!("{digest}.chunk"))
}

fn portable_path(value: &str) -> Result<PathBuf> {
    if value.is_empty()
        || value.contains('\\')
        || value.split('/').any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || segment.contains(':')
                || segment.chars().any(char::is_control)
        })
    {
        bail!("sync path must be a non-empty portable relative path: {value}");
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("unsafe sync path: {value}");
    }
    Ok(path.to_path_buf())
}

fn validate_root_key(key: &str) -> Result<()> {
    let Some((source, kind)) = key.split_once('/') else {
        bail!("invalid catalogue root key: {key}");
    };
    if source.is_empty()
        || !source
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        || !matches!(kind, "skill" | "command")
    {
        bail!("invalid catalogue root key: {key}");
    }
    Ok(())
}

fn validate_snapshot_id(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid snapshot or chunk identifier");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_negotiation_accepts_overlap_and_rejects_incompatible_ranges() {
        assert_eq!(negotiate_protocol(1, 1).expect("v1 overlap"), 1);
        assert!(negotiate_protocol(2, 3).is_err());
        assert!(negotiate_protocol(2, 1).is_err());
    }

    #[test]
    fn portable_paths_accept_nested_files_and_reject_escape_or_host_paths() {
        assert_eq!(
            portable_path("roots/team/skill/review/SKILL.md").expect("portable path"),
            PathBuf::from("roots/team/skill/review/SKILL.md")
        );
        for unsafe_path in [
            "../secret",
            "/etc/passwd",
            "C:\\secret",
            "C:/secret",
            "roots/./skill",
            "roots//skill",
            "roots/\u{0000}/skill",
        ] {
            assert!(portable_path(unsafe_path).is_err(), "{unsafe_path}");
        }
    }
}
