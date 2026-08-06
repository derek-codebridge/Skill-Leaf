use crate::{CatalogEntry, EntryKind, TrustLevel};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn tokens(value: &str) -> BTreeSet<String> {
    const STOP: &[&str] = &[
        "about", "agent", "and", "for", "from", "into", "skill", "skills", "the", "this", "use",
        "when", "with",
    ];
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|token| token.len() >= 3 && !STOP.contains(token))
        .map(ToOwned::to_owned)
        .collect()
}

pub(crate) fn match_score(
    entry: &CatalogEntry,
    task_tokens: &BTreeSet<String>,
    entries: &[CatalogEntry],
) -> (u32, BTreeSet<String>) {
    let name_tokens = tokens(&entry.name);
    let alias_tokens = entry
        .aliases
        .iter()
        .flat_map(|alias| tokens(alias))
        .collect::<BTreeSet<_>>();
    let description_tokens = tokens(&entry.description);
    let name_matches = task_tokens
        .iter()
        .filter(|token| name_tokens.contains(*token))
        .count() as u32;
    let description_matches = task_tokens
        .iter()
        .filter(|token| description_tokens.contains(*token))
        .count() as u32;
    let alias_matches = task_tokens
        .iter()
        .filter(|token| alias_tokens.contains(*token))
        .count() as u32;
    let mut reasons = BTreeSet::new();
    if name_matches > 0 {
        reasons.insert("exact name match".to_string());
    }
    if alias_matches > 0 {
        reasons.insert("exact alias match".to_string());
    }
    if name_matches == 0
        && alias_matches == 0
        && let Some(token) = unique_typo_token(entry, task_tokens, entries)
    {
        reasons.insert(format!("unique typo match: {token}"));
        return (8 + description_matches * 2, reasons);
    }
    if name_matches == 0 && alias_matches == 0 && description_matches < 2 {
        return (0, reasons);
    }
    if description_matches > 0 {
        reasons.insert("description match".to_string());
    }
    (
        name_matches * 24 + alias_matches * 20 + description_matches * 2,
        reasons,
    )
}

pub(crate) fn validate_aliases(entries: &[CatalogEntry]) -> Result<()> {
    let mut aliases = BTreeMap::<String, String>::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind != EntryKind::Resource)
    {
        for alias in &entry.aliases {
            let normalized = tokens(alias).into_iter().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                bail!("empty routing alias on {}", entry.selector());
            }
            if let Some(previous) = aliases.insert(normalized.clone(), entry.selector())
                && previous != entry.selector()
            {
                bail!(
                    "routing alias collision '{normalized}' between {previous} and {}",
                    entry.selector()
                );
            }
        }
    }
    Ok(())
}

fn unique_typo_token(
    entry: &CatalogEntry,
    task_tokens: &BTreeSet<String>,
    entries: &[CatalogEntry],
) -> Option<String> {
    let candidate_tokens = tokens(&entry.name)
        .into_iter()
        .chain(entry.aliases.iter().flat_map(|alias| tokens(alias)))
        .collect::<BTreeSet<_>>();
    for task_token in task_tokens.iter().filter(|token| token.len() >= 5) {
        if !candidate_tokens
            .iter()
            .any(|candidate| edit_distance_at_most_one(task_token, candidate))
        {
            continue;
        }
        let matching = entries
            .iter()
            .filter(|candidate| {
                candidate.kind != EntryKind::Resource && candidate.trust == TrustLevel::Trusted
            })
            .filter(|candidate| {
                tokens(&candidate.name)
                    .into_iter()
                    .chain(candidate.aliases.iter().flat_map(|alias| tokens(alias)))
                    .any(|token| edit_distance_at_most_one(task_token, &token))
            })
            .map(CatalogEntry::selector)
            .collect::<BTreeSet<_>>();
        if matching.len() == 1 && matching.contains(&entry.selector()) {
            return Some(task_token.clone());
        }
    }
    None
}

fn edit_distance_at_most_one(left: &str, right: &str) -> bool {
    if left == right || left.len().abs_diff(right.len()) > 1 {
        return left == right;
    }
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;
    while i < left.len() && j < right.len() {
        if left.get(i) == right.get(j) {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        match left.len().cmp(&right.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                if i + 1 < left.len()
                    && j + 1 < right.len()
                    && left.get(i) == right.get(j + 1)
                    && left.get(i + 1) == right.get(j)
                {
                    i += 2;
                    j += 2;
                } else {
                    i += 1;
                    j += 1;
                }
            }
        }
    }
    edits + usize::from(i < left.len() || j < right.len()) <= 1
}
