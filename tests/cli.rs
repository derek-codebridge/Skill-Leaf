use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn example_workflow_runs_end_to_end() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let catalog = temp.path().join("skillleaf.json");
    let usage = temp.path().join("usage.json");

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
        .env("SKILLLEAF_USAGE_FILE", &usage)
        .args([
            "read",
            "--catalog",
            catalog.to_str().expect("catalog path"),
            "--many",
            "example/skill:review",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("example/skill:review"));

    Command::cargo_bin("skillleaf")
        .expect("binary")
        .args([
            "stats",
            "--catalog",
            catalog.to_str().expect("catalog path"),
            "--usage-file",
            usage.to_str().expect("usage path"),
            "--format",
            "text",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1\texample/skill:review"))
        .stdout(predicate::str::contains("0\texample/command:finish"));

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

    Command::cargo_bin("skillleaf")
        .expect("binary")
        .env("SKILLLEAF_CATALOG", &catalog)
        .args(["doctor"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS: 4 entries verified"));
}
