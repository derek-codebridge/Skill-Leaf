use assert_cmd::Command;
use predicates::prelude::*;
use skillleaf::{
    PullOptions, TrustLevel, list_sync_versions, load_catalog, load_domain_registry,
    publish_snapshot, pull_snapshot, rollback_sync_snapshot, sync_status,
};

fn build_example_catalog(temp: &tempfile::TempDir) -> anyhow::Result<std::path::PathBuf> {
    let catalog = temp.path().join("source-catalog.json");
    Command::cargo_bin("skillleaf")?
        .env_remove("CODEBRIDGE_LICENSE")
        .env_remove("CODEBRIDGE_LICENSE_KEY")
        .args([
            "index",
            "--skills",
            "example=examples/skills",
            "--commands",
            "example=examples/commands",
            "--output",
            catalog.to_str().expect("catalog path"),
        ])
        .assert()
        .success();
    Ok(catalog)
}

#[test]
fn publish_pull_status_and_offline_fallback_work_end_to_end() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let remote = temp.path().join("shared");
    let destination = temp.path().join("local");
    let registry = temp.path().join("domains.json");
    let catalog = build_example_catalog(&temp)?;

    let publish = Command::cargo_bin("skillleaf")?
        .env_remove("CODEBRIDGE_LICENSE")
        .env_remove("CODEBRIDGE_LICENSE_KEY")
        .args([
            "sync",
            "publish",
            "--catalog",
            catalog.to_str().expect("catalog path"),
            "--remote",
            remote.to_str().expect("remote path"),
            "--chunk-bytes",
            "32",
        ])
        .output()?;
    assert!(publish.status.success(), "{:?}", publish);
    let manifest: serde_json::Value = serde_json::from_slice(&publish.stdout)?;
    let snapshot = manifest["snapshot_id"]
        .as_str()
        .expect("snapshot id")
        .to_string();
    assert!(
        manifest["files"]
            .as_array()
            .expect("files")
            .iter()
            .any(|file| file["chunks"].as_array().expect("chunks").len() > 1),
        "small test chunks should exercise real chunking"
    );

    Command::cargo_bin("skillleaf")?
        .env_remove("CODEBRIDGE_LICENSE")
        .env_remove("CODEBRIDGE_LICENSE_KEY")
        .args([
            "sync",
            "pull",
            "--remote",
            remote.to_str().expect("remote path"),
            "--destination",
            destination.to_str().expect("destination path"),
            "--domain",
            "shared",
            "--registry",
            registry.to_str().expect("registry path"),
            "--expected-snapshot",
            &snapshot,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"trusted\": true"));

    let status = sync_status(&remote, &destination, "shared")?;
    assert!(status.remote_available);
    assert!(status.local_verified);
    assert!(!status.update_available);
    assert!(status.fallback_ready);

    std::fs::rename(&remote, temp.path().join("shared-offline"))?;
    let fallback = pull_snapshot(
        &remote,
        &destination,
        &registry,
        "shared",
        &PullOptions {
            allow_offline_fallback: true,
            ..PullOptions::default()
        },
    )?;
    assert_eq!(fallback.mode, "offline_fallback");
    assert_eq!(fallback.snapshot_id, snapshot);
    Ok(())
}

#[test]
fn unpinned_remote_is_downgraded_and_tampered_chunks_fail_closed() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let remote = temp.path().join("shared");
    let destination = temp.path().join("local");
    let registry = temp.path().join("domains.json");
    let catalog = build_example_catalog(&temp)?;
    let manifest = publish_snapshot(&catalog, &remote, 32)?;

    let receipt = pull_snapshot(
        &remote,
        &destination,
        &registry,
        "untrusted",
        &PullOptions::default(),
    )?;
    assert!(!receipt.trusted);
    let imported = load_catalog(&receipt.catalog_path)?;
    assert!(
        imported
            .entries
            .iter()
            .all(|entry| entry.trust == TrustLevel::Untrusted)
    );

    let chunk = &manifest.files[0].chunks[0].sha256;
    let chunk_path = remote
        .join("chunks")
        .join(&chunk[..2])
        .join(format!("{chunk}.chunk"));
    std::fs::write(chunk_path, b"tampered")?;
    let error = pull_snapshot(
        &remote,
        &temp.path().join("tampered-local"),
        &temp.path().join("tampered-domains.json"),
        "tampered",
        &PullOptions {
            expected_snapshot: Some(manifest.snapshot_id),
            ..PullOptions::default()
        },
    )
    .expect_err("tampered chunks must fail");
    assert!(error.to_string().contains("failed verification"));
    Ok(())
}

#[test]
fn verified_versions_can_be_listed_and_rolled_back() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let remote = temp.path().join("shared");
    let destination = temp.path().join("local");
    let registry = temp.path().join("domains.json");
    let catalog_path = build_example_catalog(&temp)?;

    let first = publish_snapshot(&catalog_path, &remote, 64)?;
    pull_snapshot(
        &remote,
        &destination,
        &registry,
        "versioned",
        &PullOptions {
            expected_snapshot: Some(first.snapshot_id.clone()),
            ..PullOptions::default()
        },
    )?;

    let updated_skills = temp.path().join("updated-skills");
    let updated_commands = temp.path().join("updated-commands");
    std::fs::create_dir_all(updated_skills.join("helper"))?;
    std::fs::create_dir_all(updated_skills.join("review"))?;
    std::fs::create_dir_all(&updated_commands)?;
    for (source, target) in [
        (
            "examples/skills/helper/SKILL.md",
            updated_skills.join("helper/SKILL.md"),
        ),
        (
            "examples/skills/review/SKILL.md",
            updated_skills.join("review/SKILL.md"),
        ),
        (
            "examples/skills/review/checklist.md",
            updated_skills.join("review/checklist.md"),
        ),
        (
            "examples/commands/finish.md",
            updated_commands.join("finish.md"),
        ),
    ] {
        std::fs::copy(source, target)?;
    }
    let changed = updated_skills.join("review/SKILL.md");
    let mut changed_body = std::fs::read(&changed)?;
    changed_body.extend_from_slice(b"\nUpdated version.\n");
    std::fs::write(changed, changed_body)?;

    Command::cargo_bin("skillleaf")?
        .env_remove("CODEBRIDGE_LICENSE")
        .env_remove("CODEBRIDGE_LICENSE_KEY")
        .arg("index")
        .arg("--skills")
        .arg(format!("example={}", updated_skills.display()))
        .arg("--commands")
        .arg(format!("example={}", updated_commands.display()))
        .arg("--output")
        .arg(&catalog_path)
        .assert()
        .success();
    let second = publish_snapshot(&catalog_path, &remote, 64)?;
    pull_snapshot(
        &remote,
        &destination,
        &registry,
        "versioned",
        &PullOptions {
            expected_snapshot: Some(second.snapshot_id.clone()),
            ..PullOptions::default()
        },
    )?;

    let versions = list_sync_versions(&destination, "versioned")?;
    assert_eq!(versions.len(), 2);
    assert_eq!(versions.iter().filter(|version| version.active).count(), 1);
    assert!(
        versions
            .iter()
            .any(|version| version.snapshot_id == first.snapshot_id && !version.active)
    );

    let receipt = rollback_sync_snapshot(&destination, &registry, "versioned", &first.snapshot_id)?;
    assert_eq!(receipt.mode, "rollback");
    assert_eq!(receipt.snapshot_id, first.snapshot_id);
    let domains = load_domain_registry(&registry)?;
    assert_eq!(domains.domains["versioned"].catalog, receipt.catalog_path);
    assert!(
        list_sync_versions(&destination, "versioned")?
            .iter()
            .any(|version| version.snapshot_id == receipt.snapshot_id && version.active)
    );
    Ok(())
}

#[test]
fn rollback_rejects_invalid_or_missing_snapshot_ids() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let error = rollback_sync_snapshot(
        &temp.path().join("local"),
        &temp.path().join("domains.json"),
        "versioned",
        "../escape",
    )
    .expect_err("invalid identifiers must fail");
    assert!(
        error
            .to_string()
            .contains("invalid snapshot or chunk identifier")
    );

    let error = rollback_sync_snapshot(
        &temp.path().join("local"),
        &temp.path().join("domains.json"),
        "versioned",
        &"0".repeat(64),
    )
    .expect_err("missing snapshots must fail");
    assert!(error.to_string().contains("not available for rollback"));
    Ok(())
}

#[test]
fn snapshot_pin_rejects_an_unexpected_update() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let remote = temp.path().join("shared");
    let catalog = build_example_catalog(&temp)?;
    publish_snapshot(&catalog, &remote, 64)?;

    let error = pull_snapshot(
        &remote,
        &temp.path().join("local"),
        &temp.path().join("domains.json"),
        "pinned",
        &PullOptions {
            expected_snapshot: Some("0".repeat(64)),
            ..PullOptions::default()
        },
    )
    .expect_err("incorrect pins must fail");
    assert!(
        error
            .to_string()
            .contains("does not match the pinned snapshot")
    );
    Ok(())
}
