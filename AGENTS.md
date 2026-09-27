# Skill-Leaf agent notes

## Code Review Rules

Skill-Leaf is a public, local-only Rust CLI with a loopback dashboard. It indexes agent skills and commands, selects a bounded set for each task, hydrates them in one call, migrates libraries into domains, and syncs catalogues through filesystem remotes. People install it because its security boundaries and deterministic output can be trusted.

### Severity in this repo

- P0: a trust boundary weakens (untrusted entries route automatically, `trust: trusted` upgrades an untrusted source, or a hash check is skipped); path traversal, symlink following or root escape during hydration, migration or sync; bundled scripts executed; the dashboard accepting non-loopback binds or state changes without the `X-SkillLeaf-Request` header; prompts or task text written anywhere; migration or rollback deleting, moving or overwriting the user's original folders.
- P1: output order that changes between runs; typo recovery that picks among ambiguous matches; a missing dependency or source collision that does not fail indexing; the sync pointer moving before its snapshot is durable; a documented CLI or JSON receipt field changing without a version bump; a Windows path case that breaks.

### Rules

- Every hash (catalogue, body, chunk, manifest, snapshot) is verified before use and fails closed.
- Hydration reads only regular files under the indexed root. It rejects traversal, symlinks, non-regular files, oversized or non-UTF-8 input, and hidden control or bidirectional override characters.
- Writes are atomic (temporary file, then rename). `current.json` moves only after every chunk of the snapshot is durable. A pull without a pinned snapshot downgrades every imported entry to untrusted.
- Routing is deterministic and uses ordered maps. Exact names and aliases always outrank typo recovery. Typo recovery allows one edit, only for tokens of at least five ASCII characters, and only when exactly one trusted entry matches. No regex, embedding or model-based routing.
- A request opens exactly one catalogue; domains are never merged.
- Usage files store selectors, hashes, counts and timestamps only.
- Migration never deletes or moves the originals, and rollback refuses to remove anything that changed after apply.
- The dashboard binds to loopback only, caps JSON bodies at 16 KiB, checks the request header before parsing the body, and ships no external assets.
- Performance or token-saving claims in the README stay tied to a reproducible measurement.

### Not worth flagging

- Formatting (rustfmt and clippy run in CI) and Dependabot version bumps without behaviour change.
