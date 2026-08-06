use skillleaf::{
    DomainConfig, DomainRegistry, EntryKind, SourceRoot, build_catalog, domain_catalog_path,
    load_domain_registry, resolve, write_catalog_atomic, write_domain_registry_atomic,
};

#[test]
fn domains_select_one_catalog_without_cross_domain_taint() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let registry_path = temp.path().join("domains.json");
    let mut registry = DomainRegistry::default();
    for (domain, skill) in [("work", "deploy"), ("home", "garden")] {
        let root = temp.path().join(domain).join("skills").join(skill);
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {skill}\ndescription: Handle {skill} tasks.\n---\n"),
        )?;
        let catalog = build_catalog(
            &[SourceRoot {
                name: domain.to_string(),
                kind: EntryKind::Skill,
                path: temp.path().join(domain).join("skills"),
            }],
            256 * 1024,
        )?;
        let catalog_path = temp.path().join(domain).join("catalog.json");
        write_catalog_atomic(&catalog, &catalog_path)?;
        registry.domains.insert(
            domain.to_string(),
            DomainConfig {
                catalog: catalog_path,
                usage_file: None,
                account: Some(domain.to_string()),
            },
        );
    }
    write_domain_registry_atomic(&registry, &registry_path)?;
    let loaded = load_domain_registry(&registry_path)?;
    assert_eq!(loaded.domains.len(), 2);

    let work_path = domain_catalog_path(None, Some("work"), &registry_path)?;
    let work = skillleaf::load_catalog(&work_path)?;
    assert_eq!(resolve(&work, "deploy tasks", &[], 8)?.selected.len(), 1);
    assert!(resolve(&work, "garden tasks", &[], 8)?.selected.is_empty());
    Ok(())
}

#[test]
fn explicit_catalog_and_domain_are_mutually_exclusive() {
    let error = domain_catalog_path(
        Some("catalog.json".into()),
        Some("work"),
        std::path::Path::new("domains.json"),
    )
    .expect_err("ambiguous selection must fail");
    assert!(error.to_string().contains("either --catalog or --domain"));
}
