# Skillleaf

Skillleaf is a small, local-first Rust binary that keeps large AI-agent skill and command libraries out of the prompt until they are relevant.

It builds a deterministic catalog from Markdown sources, routes a task to a bounded set of entries, closes declared and linked dependencies, and hydrates the selected bodies in one process. It does not call a model, network service, API, or MCP server.

## Why

Agent hosts commonly expose every skill description and sometimes inject entire bodies at session start. That increases startup context, compaction cost, and subagent overhead. Skillleaf instead keeps a compact hash-verified index available and loads only selected leaves.

The catalog uses ordered maps and entries for reproducible output. “B-tree” describes the ordered index and dependency traversal; it is not a claim that B-trees make Markdown parsing intrinsically faster.

## Quick start

```sh
cargo build --release

skillleaf index \
  --skills example=examples/skills \
  --commands example=examples/commands \
  --output skillleaf.json

skillleaf resolve \
  --catalog skillleaf.json \
  --task "review and finish this code change"

skillleaf read \
  --catalog skillleaf.json \
  --many example/skill:review,example/resource:review/checklist.md

skillleaf doctor --catalog skillleaf.json
```

## Input layout

Skill roots contain packages with a `SKILL.md`. Other Markdown files beneath the same package are independently addressable resources. Relative Markdown links from `SKILL.md` become dependencies and hydrate with the selected skill.

Command roots contain Markdown files. Optional YAML frontmatter supports:

```yaml
---
name: finish
description: Finish a change with review and verification.
dependencies:
  - example/skill:review
---
```

## Security and integrity

- Catalog and body SHA-256 hashes fail closed.
- Hydration rejects path traversal, root escape, symlinks, non-regular files, and changed bodies.
- Inputs are size-bounded and must be UTF-8 Markdown.
- Catalog writes use an atomic temporary-file replacement.
- Source collisions and missing dependencies are errors.
- No prompts, code, credentials, or telemetry leave the computer.

## What Skillleaf deliberately does not do

- It does not install or execute skill scripts.
- It does not infer proprietary workflow policy.
- It does not use embeddings, fuzzy matching, or an LLM.
- It does not modify Claude, Codex, Cursor, or other host configuration.

Host adapters can call `resolve`, pass the selected canonical selectors to `read --many`, and inject only those returned bodies.

## Measured example

On one real local library, Skillleaf indexed 382 entries containing 3.34 MB of Markdown. A representative pull-request review task selected 101 KB and left approximately 3.24 MB (about 810,000 four-bytes-per-token estimate) out of the prompt. Across 20 warm runs, resolution averaged 4.8 ms and one-process hydration of eight bodies averaged 4.3 ms. Rebuilding the complete index averaged 96.6 ms across 10 runs.

These figures describe one Apple Silicon development machine and corpus. Run the same commands against your own library before making capacity claims.

## Integration model

A host only needs a compact instruction such as:

```text
Before loading skills, run skillleaf resolve for the task. Hydrate the returned
selectors together with skillleaf read --many. Do not preload unselected bodies.
```

The first release intentionally leaves host configuration to adapters. This keeps the core safe to embed in Claude Code, Codex, OpenCode, CI, or a custom harness without taking ownership of user settings.

## Assumptions

- Skill bodies and commands are UTF-8 Markdown.
- A skill package has one `SKILL.md`; linked Markdown resources remain beneath its package directory.
- Descriptions are routing metadata, not trusted executable policy.
- Explicit dependencies use canonical `source/kind:name` selectors.
- Deterministic lexical routing is preferable to an opaque model call for the default path. Hosts may add a separate semantic reranker without weakening hash and containment checks.

## License

MIT.
