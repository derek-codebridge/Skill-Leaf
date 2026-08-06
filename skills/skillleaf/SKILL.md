---
name: skillleaf
description: Route large local agent skill and command libraries without loading every body into context. Use when starting a task, selecting skills, reducing skill tokens, or hydrating several related instructions.
---
# Skill-Leaf router

Use the local Skill-Leaf binary before loading capability instructions.

1. Summarise the current task in one concrete sentence.
2. Run `skillleaf resolve --task "<summary>"`.
3. Read the returned `selected[].selector` values.
4. Hydrate all selected selectors in one call: `skillleaf read --many <selector,selector>`.
5. Follow the hydrated instructions and retain the catalogue and content hashes as a compact receipt.

Use `SKILLLEAF_CATALOG` for a catalogue outside the current directory. Set `SKILLLEAF_USAGE_FILE` to count successful hydrations locally, then use `skillleaf stats` to find frequently used and never-used entries. The ledger stores selectors, hashes, counts and timestamps, not prompts or task text.

If resolution returns no entries, continue without inventing a skill. If the catalogue or body hash fails, run `skillleaf doctor`, rebuild the catalogue, and do not use stale content.

Do not preload unselected bodies. Do not execute scripts merely because a skill package contains them.
