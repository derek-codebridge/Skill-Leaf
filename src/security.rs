use crate::{Catalog, EntryKind, SkillleafResult};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    #[default]
    Trusted,
    Untrusted,
}

impl TrustLevel {
    pub(crate) fn is_trusted(&self) -> bool {
        *self == Self::Trusted
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PolicyFinding {
    pub selector: String,
    pub severity: &'static str,
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct PolicyReport {
    pub schema: &'static str,
    pub catalog_sha256: String,
    pub passed: bool,
    pub findings: Vec<PolicyFinding>,
}

pub fn inspect_catalog(catalog: &Catalog, strict: bool) -> SkillleafResult<PolicyReport> {
    let mut findings = Vec::new();
    for entry in &catalog.entries {
        if entry.kind == EntryKind::Resource {
            continue;
        }
        if entry.trust == TrustLevel::Untrusted {
            findings.push(PolicyFinding {
                selector: entry.selector(),
                severity: "warning",
                code: "untrusted_source",
                message: "Automatic routing is disabled; explicit selection and --allow-untrusted are required.".to_string(),
            });
        }
        if entry.capabilities.is_empty() {
            findings.push(PolicyFinding {
                selector: entry.selector(),
                severity: "advisory",
                code: "capabilities_undeclared",
                message: "Declare capabilities when instructions require shell, network, write, secrets, or deployment access.".to_string(),
            });
        }
    }
    findings.sort_by(|left, right| {
        left.selector
            .cmp(&right.selector)
            .then_with(|| left.code.cmp(right.code))
    });
    let passed = !strict || findings.iter().all(|finding| finding.severity != "error");
    Ok(PolicyReport {
        schema: "skillleaf.policy-report.v1",
        catalog_sha256: catalog.catalog_sha256.clone(),
        passed,
        findings,
    })
}

pub(crate) fn validate_instruction_text(path: &Path, body: &str) -> Result<()> {
    for character in body.chars() {
        let disallowed_control = character.is_control() && !matches!(character, '\n' | '\r' | '\t');
        let bidi_override = matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        );
        if disallowed_control || bidi_override {
            bail!(
                "instruction contains a hidden control or bidirectional override in {}",
                path.display()
            );
        }
    }
    Ok(())
}
