use skillleaf::{
    EntryKind, SourceRoot, build_catalog, doctor, hydrate, load_catalog, resolve,
    write_catalog_atomic,
};
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
    doctor(&load_catalog(&path)?)?;
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
