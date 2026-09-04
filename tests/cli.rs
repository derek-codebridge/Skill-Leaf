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

#[test]
fn text_output_marks_only_untrusted_entries() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skills = temp.path().join("skills");
    for (name, trust) in [("safe", "trusted"), ("risky", "untrusted")] {
        let skill = skills.join(name);
        std::fs::create_dir_all(&skill)?;
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Review instructions.\ntrust: {trust}\n---\n"),
        )?;
    }
    let catalog = temp.path().join("skillleaf.json");
    Command::cargo_bin("skillleaf")?
        .args([
            "index",
            "--skills",
            &format!("example={}", skills.display()),
            "--output",
            catalog.to_str().expect("catalog path"),
        ])
        .assert()
        .success();

    for subcommand in ["resolve", "read"] {
        let mut risky = Command::cargo_bin("skillleaf")?;
        risky.args([
            subcommand,
            "--catalog",
            catalog.to_str().expect("catalog path"),
        ]);
        if subcommand == "resolve" {
            risky.args(["--task", "", "--require", "example/skill:risky"]);
        } else {
            risky.args(["--many", "example/skill:risky", "--allow-untrusted"]);
        }
        risky
            .args(["--format", "text"])
            .assert()
            .success()
            .stdout(predicate::str::contains("UNTRUSTED"));

        let mut safe = Command::cargo_bin("skillleaf")?;
        safe.args([
            subcommand,
            "--catalog",
            catalog.to_str().expect("catalog path"),
        ]);
        if subcommand == "resolve" {
            safe.args(["--task", "", "--require", "example/skill:safe"]);
        } else {
            safe.args(["--many", "example/skill:safe"]);
        }
        safe.args(["--format", "text"])
            .assert()
            .success()
            .stdout(predicate::str::contains("UNTRUSTED").not());
    }
    Ok(())
}

#[test]
fn resolve_defaults_to_three_and_honours_an_explicit_limit() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let skills = temp.path().join("skills");
    for name in ["alpha", "beta", "gamma", "delta"] {
        let skill = skills.join(name);
        std::fs::create_dir_all(&skill)?;
        std::fs::write(
            skill.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: Review validation release workflow.\n---\n# {name}\n"
            ),
        )?;
    }
    let catalog = temp.path().join("skillleaf.json");
    Command::cargo_bin("skillleaf")?
        .args([
            "index",
            "--skills",
            &format!("example={}", skills.display()),
            "--output",
            catalog.to_str().expect("catalog path"),
        ])
        .assert()
        .success();

    let default_output = Command::cargo_bin("skillleaf")?
        .args([
            "resolve",
            "--catalog",
            catalog.to_str().expect("catalog path"),
            "--task",
            "review validation release workflow",
        ])
        .output()?;
    assert!(default_output.status.success());
    let default_resolution: serde_json::Value = serde_json::from_slice(&default_output.stdout)?;
    assert_eq!(
        default_resolution["selected"]
            .as_array()
            .expect("selected array")
            .len(),
        3
    );

    let explicit_output = Command::cargo_bin("skillleaf")?
        .args([
            "resolve",
            "--catalog",
            catalog.to_str().expect("catalog path"),
            "--task",
            "review validation release workflow",
            "--limit",
            "4",
        ])
        .output()?;
    assert!(explicit_output.status.success());
    let explicit_resolution: serde_json::Value = serde_json::from_slice(&explicit_output.stdout)?;
    assert_eq!(
        explicit_resolution["selected"]
            .as_array()
            .expect("selected array")
            .len(),
        4
    );
    Ok(())
}
