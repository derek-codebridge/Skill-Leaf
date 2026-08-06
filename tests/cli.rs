use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn example_workflow_runs_end_to_end() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let catalog = temp.path().join("skillleaf.json");

    Command::cargo_bin("skillleaf")
        .expect("binary")
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
        .success()
        .stdout(predicate::str::contains("indexed 4 entries"));

    Command::cargo_bin("skillleaf")
        .expect("binary")
        .args([
            "resolve",
            "--catalog",
            catalog.to_str().expect("catalog path"),
            "--task",
            "finish and review this code change",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("example/skill:review"))
        .stdout(predicate::str::contains(
            "example/resource:review/checklist.md",
        ));

    Command::cargo_bin("skillleaf")
        .expect("binary")
        .args([
            "doctor",
            "--catalog",
            catalog.to_str().expect("catalog path"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 4 entries verified"));
}
