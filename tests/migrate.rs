use skillleaf::{
    EntryKind, HostKind, SourceRoot, apply_migration, apply_migration_with_receipt,
    load_domain_registry, plan_migration, rollback_migration,
};

#[test]
fn migration_is_non_destructive_domain_isolated_and_reversible() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skills = temp.path().join("source/skills/review");
    let commands = temp.path().join("source/commands");
    std::fs::create_dir_all(&skills)?;
    std::fs::create_dir_all(&commands)?;
    std::fs::write(
        skills.join("SKILL.md"),
        "---\nname: review\ndescription: Review changes.\n---\n",
    )?;
    std::fs::write(commands.join("finish.md"), "# Finish\n")?;
    let original_command = commands.join("finish.md");
    let registry = temp.path().join("config/domains.json");
    let host_root = temp.path().join("host");
    let plan = plan_migration(
        &[
            SourceRoot {
                name: "personal".to_string(),
                kind: EntryKind::Skill,
                path: temp.path().join("source/skills"),
            },
            SourceRoot {
                name: "personal".to_string(),
                kind: EntryKind::Command,
                path: commands.clone(),
            },
        ],
        temp.path().join("library"),
        "work".to_string(),
        registry.clone(),
        HostKind::Claude,
        host_root.clone(),
    )?;
    let receipt = apply_migration(&plan)?;
    assert!(
        original_command.exists(),
        "native slash command must remain in place"
    );
    assert!(receipt.adapter_path.exists());
    assert!(receipt.domain_root.join("catalog.json").exists());
    assert!(
        load_domain_registry(&registry)?
            .domains
            .contains_key("work")
    );

    rollback_migration(&receipt)?;
    assert!(original_command.exists());
    assert!(!receipt.domain_root.exists());
    assert!(!registry.exists());
    assert!(!receipt.adapter_path.exists());
    Ok(())
}

#[test]
fn migration_refuses_source_drift_after_plan() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skill = temp.path().join("skills/review");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(skill.join("SKILL.md"), "# Review\n")?;
    let plan = plan_migration(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        temp.path().join("library"),
        "work".to_string(),
        temp.path().join("domains.json"),
        HostKind::Codex,
        temp.path().join("host"),
    )?;
    std::fs::write(skill.join("SKILL.md"), "# Changed\n")?;
    let error = apply_migration(&plan).expect_err("source drift must invalidate plan");
    assert!(error.to_string().contains("changed after planning"));
    Ok(())
}

#[test]
fn migration_rejects_tampered_plan() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skill = temp.path().join("skills/review");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(skill.join("SKILL.md"), "# Review\n")?;
    let mut plan = plan_migration(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        temp.path().join("library"),
        "work".to_string(),
        temp.path().join("domains.json"),
        HostKind::Codex,
        temp.path().join("host"),
    )?;
    plan.domain = "other".to_string();
    let error = apply_migration(&plan).expect_err("tampered plan must fail");
    assert!(error.to_string().contains("plan hash mismatch"));
    Ok(())
}

#[test]
fn adapter_conflict_leaves_no_partial_registry_or_domain() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skill = temp.path().join("skills/review");
    let adapter = temp.path().join("host/skills/skillleaf/SKILL.md");
    std::fs::create_dir_all(&skill)?;
    std::fs::create_dir_all(adapter.parent().expect("adapter parent"))?;
    std::fs::write(skill.join("SKILL.md"), "# Review\n")?;
    std::fs::write(&adapter, "custom adapter")?;
    let registry = temp.path().join("domains.json");
    let destination = temp.path().join("library");
    let plan = plan_migration(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        destination.clone(),
        "work".to_string(),
        registry.clone(),
        HostKind::Codex,
        temp.path().join("host"),
    )?;
    let error = apply_migration(&plan).expect_err("adapter conflict must fail");
    assert!(error.to_string().contains("different content"));
    assert!(!registry.exists());
    assert!(!destination.join("domains/work").exists());
    assert_eq!(std::fs::read_to_string(adapter)?, "custom adapter");
    Ok(())
}

#[test]
fn receipt_write_failure_rolls_back_apply() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skill = temp.path().join("skills/review");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(skill.join("SKILL.md"), "# Review\n")?;
    let registry = temp.path().join("domains.json");
    let destination = temp.path().join("library");
    let plan = plan_migration(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        destination.clone(),
        "work".to_string(),
        registry.clone(),
        HostKind::Codex,
        temp.path().join("host"),
    )?;
    let invalid_receipt = temp.path().join("receipt-directory");
    std::fs::create_dir_all(&invalid_receipt)?;
    let error = apply_migration_with_receipt(&plan, &invalid_receipt)
        .expect_err("receipt failure must roll back");
    assert!(error.to_string().contains("migration was rolled back"));
    assert!(!registry.exists());
    assert!(!destination.join("domains/work").exists());
    Ok(())
}

#[test]
fn destination_inside_source_is_rejected() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skill = temp.path().join("skills/review");
    std::fs::create_dir_all(&skill)?;
    std::fs::write(skill.join("SKILL.md"), "# Review\n")?;
    let error = plan_migration(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        temp.path().join("skills/generated"),
        "work".to_string(),
        temp.path().join("domains.json"),
        HostKind::Codex,
        temp.path().join("host"),
    )
    .expect_err("recursive destination must fail");
    assert!(error.to_string().contains("destination cannot be inside"));
    Ok(())
}
