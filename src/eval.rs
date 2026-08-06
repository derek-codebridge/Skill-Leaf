use crate::{Catalog, EntryKind, SkillleafError, SkillleafResult, resolve};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

pub const EVAL_SCHEMA: &str = "skillleaf.eval.v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalSuite {
    pub schema: String,
    pub cases: Vec<EvalCase>,
    #[serde(default = "default_min_recall")]
    pub min_recall: f64,
    #[serde(default = "default_min_precision")]
    pub min_precision: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EvalCase {
    pub name: String,
    pub task: String,
    #[serde(default)]
    pub expect: Vec<String>,
    #[serde(default)]
    pub reject: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvalCaseResult {
    pub name: String,
    pub passed: bool,
    pub selected: Vec<String>,
    pub missed: Vec<String>,
    pub rejected_but_selected: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvalReport {
    pub schema: &'static str,
    pub catalog_sha256: String,
    pub passed: bool,
    pub recall: f64,
    pub precision: f64,
    pub cases: Vec<EvalCaseResult>,
}

pub fn load_eval_suite(path: &Path) -> SkillleafResult<EvalSuite> {
    load_suite(path).map_err(|error| SkillleafError::CatalogInput(format!("{error:#}")))
}

fn load_suite(path: &Path) -> Result<EvalSuite> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let suite: EvalSuite = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid evaluation JSON in {}", path.display()))?;
    if suite.schema != EVAL_SCHEMA {
        bail!("unsupported evaluation schema: {}", suite.schema);
    }
    if suite.cases.is_empty() {
        bail!("evaluation suite must contain at least one case");
    }
    if !(0.0..=1.0).contains(&suite.min_recall) || !(0.0..=1.0).contains(&suite.min_precision) {
        bail!("evaluation thresholds must be between 0 and 1");
    }
    Ok(suite)
}

pub fn evaluate(catalog: &Catalog, suite: &EvalSuite, limit: usize) -> SkillleafResult<EvalReport> {
    let mut case_results = Vec::new();
    let mut expected_total = 0usize;
    let mut selected_total = 0usize;
    let mut true_positive_total = 0usize;
    let routable = catalog
        .entries
        .iter()
        .filter(|entry| entry.kind != EntryKind::Resource)
        .map(|entry| entry.selector())
        .collect::<BTreeSet<_>>();
    for case in &suite.cases {
        let resolution = resolve(catalog, &case.task, &[], limit)?;
        let selected = resolution
            .selected
            .into_iter()
            .map(|entry| entry.selector)
            .filter(|selector| routable.contains(selector))
            .collect::<BTreeSet<_>>();
        let expected = case.expect.iter().cloned().collect::<BTreeSet<_>>();
        let rejected = case.reject.iter().cloned().collect::<BTreeSet<_>>();
        let missed = expected.difference(&selected).cloned().collect::<Vec<_>>();
        let rejected_but_selected = rejected
            .intersection(&selected)
            .cloned()
            .collect::<Vec<_>>();
        expected_total += expected.len();
        selected_total += selected.len();
        true_positive_total += expected.intersection(&selected).count();
        case_results.push(EvalCaseResult {
            name: case.name.clone(),
            passed: missed.is_empty() && rejected_but_selected.is_empty(),
            selected: selected.into_iter().collect(),
            missed,
            rejected_but_selected,
        });
    }
    let recall = ratio(true_positive_total, expected_total);
    let precision = ratio(true_positive_total, selected_total);
    let passed = case_results.iter().all(|case| case.passed)
        && recall >= suite.min_recall
        && precision >= suite.min_precision;
    Ok(EvalReport {
        schema: "skillleaf.eval-report.v1",
        catalog_sha256: catalog.catalog_sha256.clone(),
        passed,
        recall,
        precision,
        cases: case_results,
    })
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn default_min_recall() -> f64 {
    1.0
}

fn default_min_precision() -> f64 {
    0.0
}
