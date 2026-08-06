use skillleaf::{EntryKind, SourceRoot, build_catalog, hydrate, record_hydrations, usage_report};

#[test]
fn concurrent_hydrations_are_not_lost() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = std::path::PathBuf::from("examples/skills");
    let catalog = build_catalog(
        &[SourceRoot {
            name: "example".to_string(),
            kind: EntryKind::Skill,
            path: root,
        }],
        256 * 1024,
    )
    .expect("catalog");
    let selector = "example/skill:review".to_string();
    let hydrated = hydrate(&catalog, std::slice::from_ref(&selector)).expect("hydrate");
    let usage = temp.path().join("usage.json");

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let hydrated = &hydrated;
            let usage = &usage;
            scope.spawn(move || record_hydrations(usage, hydrated).expect("record usage"));
        }
    });

    let report = usage_report(&catalog, &usage).expect("usage report");
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.selector == selector)
        .expect("reported selector");
    assert_eq!(entry.hydrate_count, 8);
}

#[test]
fn malformed_usage_ledger_fails_closed() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = std::path::PathBuf::from("examples/skills");
    let catalog = build_catalog(
        &[SourceRoot {
            name: "example".to_string(),
            kind: EntryKind::Skill,
            path: root,
        }],
        256 * 1024,
    )
    .expect("catalog");
    let usage = temp.path().join("usage.json");
    std::fs::write(&usage, b"not-json").expect("write invalid usage ledger");

    let error = usage_report(&catalog, &usage).expect_err("malformed usage must fail");
    assert!(error.to_string().contains("invalid usage ledger JSON"));
}
