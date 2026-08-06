use skillleaf::{
    BuildOptions, EntryKind, SourceRoot, build_catalog, build_catalog_with_options, doctor,
    hydrate, hydrate_with_policy, load_catalog, resolve, write_catalog_atomic,
};
use std::collections::BTreeSet;
use std::fs;
use tempfile::TempDir;

fn fixture() -> anyhow::Result<(TempDir, std::path::PathBuf)> {
    let temp = tempfile::tempdir()?;
    let skills = temp.path().join("skills");
    let commands = temp.path().join("commands");
    fs::create_dir_all(skills.join("review/references"))?;
    fs::create_dir_all(skills.join("helper"))?;
    fs::create_dir_all(&commands)?;
    fs::write(
        skills.join("review/SKILL.md"),
        "---\nname: review\ndescription: Review code changes for regressions and validation.\n---\n# Review\n\nUse the [checklist](references/checklist.md).\n",
    )?;
    fs::write(
        skills.join("review/references/checklist.md"),
        "# Checklist\n\nCheck tests and boundaries.\n",
    )?;
    fs::write(
        skills.join("helper/SKILL.md"),
        "---\nname: helper\ndescription: Provide unrelated general assistance.\n---\n# Helper\n",
    )?;
    fs::write(
        commands.join("finish.md"),
        "---\nname: finish\ndescription: Finish changes with review and validation.\ndependencies:\n  - fixture/skill:review\n---\n# Finish\n",
    )?;
    let catalog = build_catalog(
        &[
            SourceRoot {
                name: "fixture".to_string(),
                kind: EntryKind::Skill,
                path: temp.path().join("skills"),
            },
            SourceRoot {
                name: "fixture".to_string(),
                kind: EntryKind::Command,
                path: temp.path().join("commands"),
            },
        ],
        256 * 1024,
    )?;
    let path = temp.path().join("catalog.json");
    write_catalog_atomic(&catalog, &path)?;
    Ok((temp, path))
}

#[test]
fn resolves_and_hydrates_only_the_dependency_closure() -> anyhow::Result<()> {
    let (_temp, path) = fixture()?;
    let catalog = load_catalog(&path)?;
    let resolution = resolve(&catalog, "finish and review this change", &[], 8)?;
    let selectors = resolution
        .selected
        .iter()
        .map(|entry| entry.selector.as_str())
        .collect::<Vec<_>>();

    assert!(selectors.contains(&"fixture/command:finish"));
    assert!(selectors.contains(&"fixture/skill:review"));
    assert!(selectors.contains(&"fixture/resource:review/references/checklist.md"));
    assert!(resolution.selected_bytes < resolution.corpus_bytes);

    let hydrated = hydrate(
        &catalog,
        &resolution
            .selected
            .iter()
            .map(|entry| entry.selector.clone())
            .collect::<Vec<_>>(),
    )?;
    assert_eq!(hydrated.len(), 3);
    Ok(())
}

#[test]
fn changed_body_fails_closed() -> anyhow::Result<()> {
    let (temp, path) = fixture()?;
    let catalog = load_catalog(&path)?;
    fs::write(temp.path().join("skills/review/SKILL.md"), "changed")?;
    let error = hydrate(&catalog, &["fixture/skill:review".to_string()])
        .expect_err("changed body must fail");
    assert!(error.to_string().contains("content hash mismatch"));
    Ok(())
}

#[test]
fn tampered_catalog_fails_closed() -> anyhow::Result<()> {
    let (_temp, path) = fixture()?;
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    value["entries"][0]["description"] = serde_json::json!("tampered");
    fs::write(&path, serde_json::to_vec_pretty(&value)?)?;
    let error = load_catalog(&path).expect_err("tampered catalog must fail");
    assert!(error.to_string().contains("catalog hash mismatch"));
    Ok(())
}

#[test]
fn missing_dependency_is_rejected_during_indexing() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("commands"))?;
    fs::write(
        temp.path().join("commands/broken.md"),
        "---\nname: broken\ndescription: Broken dependency example.\ndependencies:\n  - local/skill:missing\n---\n",
    )?;
    let error = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Command,
            path: temp.path().join("commands"),
        }],
        256 * 1024,
    )
    .expect_err("missing dependency must fail");
    assert!(error.to_string().contains("missing dependency"));
    Ok(())
}

#[test]
fn doctor_verifies_the_complete_fixture() -> anyhow::Result<()> {
    let (_temp, path) = fixture()?;
    let catalog = load_catalog(&path)?;
    doctor(&catalog)?;
    assert!(!catalog.entries.is_empty());
    Ok(())
}

#[test]
fn malformed_but_common_frontmatter_has_a_deterministic_fallback() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("skills/fallback"))?;
    fs::write(
        temp.path().join("skills/fallback/SKILL.md"),
        "---\nname: fallback\ndescription: Review: validation and boundaries\n---\n# Fallback\n",
    )?;
    let catalog = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
    )?;
    assert_eq!(catalog.entries[0].name, "fallback");
    assert_eq!(
        catalog.entries[0].description,
        "Review: validation and boundaries"
    );
    Ok(())
}

#[test]
fn duplicate_source_kind_roots_fail_closed() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    for root in ["first", "second"] {
        fs::create_dir_all(temp.path().join(root).join("same"))?;
        fs::write(
            temp.path().join(root).join("same/SKILL.md"),
            "---\nname: same\ndescription: Same capability from two roots.\n---\n",
        )?;
    }
    let error = build_catalog(
        &[
            SourceRoot {
                name: "local".to_string(),
                kind: EntryKind::Skill,
                path: temp.path().join("first"),
            },
            SourceRoot {
                name: "local".to_string(),
                kind: EntryKind::Skill,
                path: temp.path().join("second"),
            },
        ],
        256 * 1024,
    )
    .expect_err("duplicate source/kind root must fail");
    assert!(error.to_string().contains("duplicate source and kind"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_skill_bodies_are_not_indexed() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("skills/linked"))?;
    fs::write(temp.path().join("outside.md"), "# Outside\n")?;
    symlink(
        temp.path().join("outside.md"),
        temp.path().join("skills/linked/SKILL.md"),
    )?;
    let catalog = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
    )?;
    assert!(catalog.entries.is_empty());
    Ok(())
}

#[test]
fn aliases_and_unique_typos_route_deterministically() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("skills/review"))?;
    fs::write(
        temp.path().join("skills/review/SKILL.md"),
        "---\nname: critical-review\ndescription: Inspect a change before release.\naliases:\n  - adversarial-review\n---\n",
    )?;
    let catalog = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
    )?;
    let exact = resolve(&catalog, "run adversarial review", &[], 8)?;
    assert_eq!(exact.selected[0].selector, "local/skill:critical-review");
    assert!(
        exact.selected[0]
            .reasons
            .contains(&"exact alias match".to_string())
    );

    let typo = resolve(&catalog, "run adversarail", &[], 8)?;
    assert_eq!(typo.selected[0].selector, "local/skill:critical-review");
    assert!(
        typo.selected[0]
            .reasons
            .iter()
            .any(|reason| reason.starts_with("unique typo match"))
    );
    Ok(())
}

#[test]
fn ambiguous_typo_does_not_auto_route() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    for name in ["deploy", "deplox"] {
        fs::create_dir_all(temp.path().join("skills").join(name))?;
        fs::write(
            temp.path().join("skills").join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: A distinct capability.\n---\n"),
        )?;
    }
    let catalog = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
    )?;
    assert!(resolve(&catalog, "deplow", &[], 8)?.selected.is_empty());
    Ok(())
}

#[test]
fn alias_collisions_fail_closed() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    for name in ["one", "two"] {
        fs::create_dir_all(temp.path().join("skills").join(name))?;
        fs::write(
            temp.path().join("skills").join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Fixture.\naliases:\n  - shared-alias\n---\n"),
        )?;
    }
    let error = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
    )
    .expect_err("ambiguous aliases must fail");
    assert!(error.to_string().contains("routing alias collision"));
    Ok(())
}

#[test]
fn untrusted_sources_require_explicit_selection_and_hydration() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("skills/remote"))?;
    fs::write(
        temp.path().join("skills/remote/SKILL.md"),
        "---\nname: remote\ndescription: Deploy remote infrastructure safely.\n---\n",
    )?;
    let catalog = build_catalog_with_options(
        &[SourceRoot {
            name: "downloaded".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
        &BuildOptions {
            untrusted_sources: BTreeSet::from(["downloaded".to_string()]),
            ..BuildOptions::default()
        },
    )?;
    assert!(
        resolve(&catalog, "deploy remote infrastructure", &[], 8)?
            .selected
            .is_empty()
    );
    let selector = "downloaded/skill:remote".to_string();
    assert_eq!(
        resolve(&catalog, "deploy", std::slice::from_ref(&selector), 8)?.selected[0].selector,
        selector
    );
    assert!(hydrate(&catalog, std::slice::from_ref(&selector)).is_err());
    assert_eq!(
        hydrate_with_policy(&catalog, std::slice::from_ref(&selector), true)?.len(),
        1
    );
    Ok(())
}

#[test]
fn trusted_default_keeps_catalog_serialization_backwards_compatible() -> anyhow::Result<()> {
    let (_temp, catalog_path) = fixture()?;
    let catalog = load_catalog(&catalog_path)?;
    let encoded = serde_json::to_value(&catalog)?;
    assert!(
        encoded["entries"]
            .as_array()
            .expect("catalog entries")
            .iter()
            .all(|entry| entry.get("trust").is_none())
    );
    Ok(())
}

#[test]
fn untrusted_root_does_not_taint_same_named_sibling_kind() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("skills/review"))?;
    fs::create_dir_all(temp.path().join("commands"))?;
    fs::write(
        temp.path().join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review changes.\n---\n",
    )?;
    fs::write(
        temp.path().join("commands/finish.md"),
        "---\nname: finish\ndescription: Finish changes.\n---\n",
    )?;
    let catalog = build_catalog_with_options(
        &[
            SourceRoot {
                name: "personal".to_string(),
                kind: EntryKind::Skill,
                path: temp.path().join("skills"),
            },
            SourceRoot {
                name: "personal".to_string(),
                kind: EntryKind::Command,
                path: temp.path().join("commands"),
            },
        ],
        256 * 1024,
        &BuildOptions {
            untrusted_roots: BTreeSet::from(["personal/skill".to_string()]),
            ..BuildOptions::default()
        },
    )?;
    assert_eq!(
        catalog
            .entries
            .iter()
            .find(|entry| entry.kind == EntryKind::Command)
            .expect("command entry")
            .trust,
        skillleaf::TrustLevel::Trusted
    );
    assert_eq!(
        catalog
            .entries
            .iter()
            .find(|entry| entry.kind == EntryKind::Skill)
            .expect("skill entry")
            .trust,
        skillleaf::TrustLevel::Untrusted
    );
    Ok(())
}

#[test]
fn hidden_direction_overrides_are_rejected() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path().join("skills/unsafe"))?;
    fs::write(
        temp.path().join("skills/unsafe/SKILL.md"),
        "---\nname: unsafe\ndescription: Hidden text.\n---\nDo this \u{202e}instead.",
    )?;
    let error = build_catalog(
        &[SourceRoot {
            name: "local".to_string(),
            kind: EntryKind::Skill,
            path: temp.path().join("skills"),
        }],
        256 * 1024,
    )
    .expect_err("display-spoofing control must fail");
    assert!(error.to_string().contains("bidirectional override"));
    Ok(())
}
