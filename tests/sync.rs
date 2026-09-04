use assert_cmd::Command;
use predicates::prelude::*;
use skillleaf::{
    PullOptions, TrustLevel, load_catalog, publish_snapshot, pull_snapshot, sync_status,
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
