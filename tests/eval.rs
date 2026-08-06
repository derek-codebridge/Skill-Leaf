use skillleaf::{EntryKind, EvalCase, EvalSuite, SourceRoot, build_catalog, evaluate};

#[test]
fn evaluation_reports_misses_and_rejected_routes() -> anyhow::Result<()> {
    let catalog = build_catalog(
        &[SourceRoot {
            name: "example".to_string(),
            kind: EntryKind::Skill,
            path: "examples/skills".into(),
        }],
        256 * 1024,
    )?;
    let passing = EvalSuite {
        schema: "skillleaf.eval.v1".to_string(),
        cases: vec![EvalCase {
            name: "review".to_string(),
            task: "review this change with a checklist".to_string(),
            expect: vec!["example/skill:review".to_string()],
            reject: vec!["example/skill:helper".to_string()],
        }],
        min_recall: 1.0,
        min_precision: 0.0,
    };
    let report = evaluate(&catalog, &passing, 8)?;
    assert!(report.passed);
    assert_eq!(report.recall, 1.0);

    let failing = EvalSuite {
        cases: vec![EvalCase {
            expect: vec!["example/skill:missing".to_string()],
            ..passing.cases[0].clone()
        }],
        ..passing
    };
    let report = evaluate(&catalog, &failing, 8)?;
    assert!(!report.passed);
    assert_eq!(report.cases[0].missed, vec!["example/skill:missing"]);
    Ok(())
}
