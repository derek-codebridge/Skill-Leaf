# Skill-Leaf

Most coding agents load every available skill description before the work begins. Some load entire skill bodies. It works, but it can spend hundreds of thousands of tokens explaining capabilities the agent never uses.

Skill-Leaf keeps those files on your computer. A small Rust binary builds a verified catalogue, selects the few entries relevant to the task, follows their linked references, then reads the selected bodies in one call. No model. No remote service. No MCP server.

We built Skill-Leaf for [Codebridge.com.au](https://codebridge.com.au) because our own agents had reached hundreds of skills and commands. We are releasing the router so other developers can test it, improve it and spend fewer tokens on repeated setup.

## What it changes

Without a router, an agent host may place the complete skill library in the prompt. Skill-Leaf leaves the library on disk and returns a small receipt:

1. `index` creates an ordered, hash-verified catalogue.
2. `resolve` selects a bounded skill and command dependency closure.
3. `read --many` hydrates those bodies in one process.
4. `eval` checks routing against deterministic fixtures.
5. `doctor` checks every path, dependency, body hash and trust boundary.

The ordered index uses Rust's B-tree maps for reproducible output. The speed comes from doing less work and reading selected files together, not from claiming that a B-tree makes Markdown parsing magically faster.

## Measured result

We tested Skill-Leaf against one real local library with 382 entries and 3.34 MB of Markdown. A representative pull request review selected 101 KB, leaving about 3.24 MB out of the prompt. Using a rough four-bytes-per-token estimate, that avoided approximately 810,000 input tokens for that request.

Across 20 warm runs on an Apple Silicon development machine, resolution averaged 4.8 ms and hydration of eight bodies averaged 4.3 ms. Rebuilding the full catalogue averaged 96.6 ms across 10 runs.

Those numbers describe one machine, library and task. Measure your own setup before making capacity or cost claims.

## Local REST dashboard

Launch the loopback-only interface, then open the printed URL:

```sh
just ui
# or: skillleaf ui --bind 127.0.0.1:8787
```

The dashboard lists the current skills and commands, shows content-hash versions, rolls back only to locally verified snapshots, pulls updates, saves immutable backups, and shares the configured snapshot location through `/api/v1/*`. State-changing requests require `X-SkillLeaf-Request: 1` before body parsing, JSON bodies are capped at 16 KiB, and non-loopback binds are rejected. The interface ships inside the Rust binary with no Node.js runtime or external assets.

The **Backup / bucket location** accepts any filesystem path. Point it at a local folder or a folder mounted or synchronised by Cloudflare R2, Amazon S3, Azure Blob Storage, OneDrive, or another object-storage filesystem bridge. Skill-Leaf does not yet collect cloud credentials or call provider APIs directly; access control and share revocation stay with the selected storage provider.

## Quick start

```sh
cargo install --path .

skillleaf index \
  --skills personal="$HOME/.claude/skills" \
  --commands personal="$HOME/.claude/commands" \
  --output "$HOME/.config/skillleaf/catalog.json"

export SKILLLEAF_CATALOG="$HOME/.config/skillleaf/catalog.json"
export SKILLLEAF_USAGE_FILE="$HOME/.config/skillleaf/usage.json"

skillleaf resolve --task "review and finish this code change" --limit 3
skillleaf read --many personal/skill:critical-review
skillleaf stats --format text
skillleaf doctor
```

Use a named domain when work, home, customer or project libraries must remain isolated:

```sh
skillleaf domain add work \
  --catalog "$HOME/.local/share/skillleaf/domains/work/catalog.json" \
  --registry "$HOME/.config/skillleaf/domains.json"

export SKILLLEAF_DOMAIN=work
export SKILLLEAF_REGISTRY="$HOME/.config/skillleaf/domains.json"
skillleaf resolve --task "review this release"
```

Skill-Leaf opens exactly one catalogue for a request. It never merges domain entries, descriptions, usage ledgers or account labels during routing.

The repository also includes a compact host-neutral router skill at [`skills/skillleaf/SKILL.md`](skills/skillleaf/SKILL.md). Install or link that one skill in your agent host. Keep the larger skill library outside the prompt.

## Install

Build from a checked-out copy:

```sh
cargo install --path . --locked
skillleaf --help
```

After the repository is public, Cargo can install the same source directly:

```sh
cargo install --git https://github.com/derek-codebridge/Skill-Leaf --locked
```

Skill-Leaf does not change Claude Code, Codex, OpenCode or your skill folders during installation. That is deliberate. Migration stays visible and reversible.

## Migrate an existing library safely

Migration is a three-step, receipt-backed operation. Planning snapshots every source file. Apply refuses source drift, copies into a new domain, verifies the resulting catalogue, installs one compact router adapter and writes a rollback receipt. It never deletes or moves the original skill or command folders.

```sh
skillleaf migrate plan \
  --skills personal="$HOME/.claude/skills" \
  --commands personal="$HOME/.claude/commands" \
  --destination "$HOME/.local/share/skillleaf" \
  --domain work \
  --registry "$HOME/.config/skillleaf/domains.json" \
  --host claude \
  --host-root "$HOME/.claude" \
  --output migration-plan.json

skillleaf migrate apply \
  --plan migration-plan.json \
  --receipt migration-receipt.json

skillleaf doctor \
  --domain work \
  --registry "$HOME/.config/skillleaf/domains.json"
```

Native slash-command files remain in the host's command directory, so `/commands` continue to work. To undo the migration, run `skillleaf migrate rollback --receipt migration-receipt.json`. Rollback refuses to remove anything if the copied domain, adapter or registry changed after apply; review those changes rather than losing them.

## Share and sync a catalogue

The filesystem federation transport works with a local folder, network mount, OneDrive folder, or an R2/S3/Azure mount or gateway. It publishes immutable content-addressed chunks and moves the small `current.json` pointer only after the complete snapshot is durable. Pulls materialise a verified generation, then atomically update the domain registry; older generations remain available as offline fallback.

```sh
skillleaf sync publish \
  --catalog "$HOME/.config/skillleaf/catalog.json" \
  --remote "$HOME/OneDrive/skillleaf-team"

# Pin the printed snapshot ID to preserve its trust metadata. Without a pin,
# every imported entry is downgraded to untrusted and cannot route automatically.
skillleaf sync pull \
  --remote "$HOME/OneDrive/skillleaf-team" \
  --destination "$HOME/.local/share/skillleaf" \
  --domain team \
  --registry "$HOME/.config/skillleaf/domains.json" \
  --expected-snapshot <sha256>

skillleaf sync status \
  --remote "$HOME/OneDrive/skillleaf-team" \
  --destination "$HOME/.local/share/skillleaf" \
  --domain team
```

Running `sync pull` again is the manual update/resync action. If the remote folder is unavailable, pull re-verifies and rebinds the last local generation by default; pass `--no-offline-fallback` to fail instead. Only indexed UTF-8 Markdown bodies are transferred—Skill-Leaf does not copy or execute scripts or binaries. Protocol ranges are negotiated explicitly, paths and sizes are bounded, and every chunk, file, manifest and catalogue is hash-verified.

Filesystem sharing inherits access and revocation from its storage provider. Revoking access prevents future pulls but cannot erase copies already downloaded. Native R2, S3 and Azure Blob APIs, authenticated share links and a small monitor GUI can be added behind this stable JSON/CLI contract; adding them now would duplicate provider authentication and increase the attack surface before the sync format has field experience.

The same commands work in PowerShell with native Windows paths. No shell-specific copy, archive or symlink command is required.

## Expected benefit

Skill-Leaf helps when the host would otherwise preload a large skill and command library. In that shape, users should see:

- smaller session-start prompts because only the router description is always visible;
- lower input-token use because irrelevant bodies stay on disk;
- faster selected-body loading because one process hydrates the complete dependency closure;
- deterministic selection and output, without an embedding service or model call;
- verified nested Markdown dependencies, loaded only when their parent skill is selected;
- one catalogue shared across Claude Code, Codex, OpenCode, CI and custom agent runners;
- local usage evidence showing which entries earn maintenance and which never get hydrated.

The host must honour the router workflow for these savings to appear. Skill-Leaf cannot stop a host from preloading files that remain in its own discovery directory. Start with `--limit 3` for ordinary work and lower it to `2` for especially narrow tasks; explicit dependencies still expand to their complete verified closure. Measure a clean session before and after migration rather than assuming the result.

## Supported layout

Skill roots contain folders with a `SKILL.md`. Markdown files beneath the same folder become separately addressable resources. Relative Markdown links from `SKILL.md` become verified dependencies.

Command roots contain Markdown files. Skill and command frontmatter supports a name, description, deterministic aliases, declared capabilities, explicit dependencies and an optional trust downgrade:

```yaml
---
name: finish
description: >-
  Finish a change with review and verification.
aliases:
  - release-finish
capabilities:
  - shell
  - write
dependencies:
  - personal/skill:critical-review
---
```

Exact name and alias matches always outrank typo recovery. Typo recovery allows one edit only for tokens of at least five ASCII characters, and only when exactly one trusted entry matches. Ambiguous typos select nothing. Explicit `--require` selectors remain exact.

Relative Markdown links and explicit `dependencies` form the verified chain. Arbitrary `@file` references are not parsed in version 0.2.0; convert important ones to Markdown links or explicit selectors so missing references fail during indexing.

## Security boundaries

- Catalogue and body SHA-256 hashes fail closed.
- Hydration rejects path traversal, root escape, symlinks, non-regular files and changed bodies.
- Inputs are size-bounded UTF-8 Markdown.
- Catalogue writes use atomic temporary-file replacement.
- Source collisions and missing dependencies are errors.
- Prompts, code and credentials stay on the computer.
- Optional usage counts store selectors, hashes, counts and timestamps locally. They never store prompts or task text.
- Skill-Leaf reads instructions. It does not execute bundled scripts.
- Hidden control characters and bidirectional display overrides are rejected while indexing.
- Sources indexed with `--untrusted-skills` or `--untrusted-commands` cannot route automatically. They require an exact `--require` and `read --allow-untrusted`.
- Skill and command frontmatter may set `trust: untrusted` to apply the same restrictions. `trust: trusted` never upgrades an untrusted source; empty or unknown values fail indexing.
- Untrusted selections and hydrations include `"trust": "untrusted"` in JSON receipts. Trusted receipts omit the default field, so the safety signal adds no normal routing tokens.
- `capabilities` declarations make requested shell, network, write, secret and deployment authority inspectable; they are evidence, not a sandbox.

Static checks cannot prove that natural-language instructions are harmless, and self-authored metadata is not proof of safety. Treat downloaded skills as untrusted at the source-root boundary, check the trust value on hydration receipts, contain untrusted bodies as passive data during review, and use the host's permission boundary for tool execution.

## Measure routing quality

Evaluation fixtures are ordinary JSON and contain task summaries plus expected and forbidden selectors. They contain no prompts or skill bodies. Start with [`examples/eval.json`](examples/eval.json):

```sh
skillleaf eval --catalog skillleaf.json --suite examples/eval.json
```

The command reports per-case misses, forbidden selections, recall and precision, then exits non-zero when the suite or configured thresholds fail. This makes description, alias and router changes testable before deployment.

## Deliberate limits

Skill-Leaf does not execute skills, grant tool permissions or infer private workflow policy. It does not use regex routing, embeddings or an LLM. Deterministic lexical routing is the dependable first path; bounded unique typo recovery only runs after exact name and alias matching.

## Use it with an agent

An agent host needs one compact instruction:

```text
Before loading skills, run skillleaf resolve --limit 3 for the task. Read the returned
selectors together with skillleaf read --many. Do not preload unselected bodies.
```

Claude Code, Codex, OpenCode, CI and custom agent runners can use the same binary. Host-specific installers can remain separate from the verified core.

### Use it alongside CodeBridge

Skill-Leaf and CodeBridge are complementary: Skill-Leaf owns standalone filesystem catalogues and host-neutral routing, while CodeBridge owns its bundled/plugin catalogue, host setup and OBY orchestration. When both are installed, keep one active router and one owner for each body; do not merge domains implicitly or count one hydration twice.
Skill-Leaf has no licence-key check, CodeBridge runtime dependency, remote service requirement or paid activation step.

Current CodeBridge setup/update prunes its managed per-skill shims and installs a single catalogue router. Keep that router as the owner of CodeBridge entries, and use standalone Skill-Leaf for separate personal or project libraries. Older CodeBridge installations that still expose managed per-skill shims should be updated. Never remove personal or modified skills merely to reduce prompt size.

## See what earns its place

Set `SKILLLEAF_USAGE_FILE` to enable local hydration counts. `skillleaf stats` lists used and never-hydrated skills and commands, so you can remove dead entries, sharpen weak descriptions and focus maintenance where agents actually work.

Counting happens only after a body passes hash and containment checks. Concurrent agent processes share a file lock and atomic write path, so increments are not silently lost. The feature is opt-in and does not send data anywhere.

## Licence

Skill-Leaf is source-available under the [MIT License with the Commons Clause License Condition v1.0](LICENSE). It is not OSI open source because selling substantially the same software is restricted.

Individuals and businesses may use, modify and run Skill-Leaf, including for internal commercial work. Selling Skill-Leaf itself, a lightly modified substitute, or a paid service whose value derives substantially from Skill-Leaf requires a [separate commercial licence](COMMERCIAL-LICENCE.md).

Contributions are welcome under the [contribution terms](CONTRIBUTOR-LICENCE-AGREEMENT.md). Names and branding remain subject to the [trademark policy](TRADEMARKS.md).
