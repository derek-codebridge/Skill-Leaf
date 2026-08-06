# Skillleaf

Most coding agents load every available skill description before the work begins. Some load entire skill bodies. It works, but it can spend hundreds of thousands of tokens explaining capabilities the agent never uses.

Skillleaf keeps those files on your computer. A small Rust binary builds a verified catalogue, selects the few entries relevant to the task, follows their linked references, then reads the selected bodies in one call. No model. No remote service. No MCP server.

We built Skillleaf for [Codebridge.com.au](https://codebridge.com.au) because our own agents had reached hundreds of skills and commands. We are releasing the router so other developers can test it, improve it and spend fewer tokens on repeated setup.

## What it changes

Without a router, an agent host may place the complete skill library in the prompt. Skillleaf leaves the library on disk and returns a small receipt:

1. `index` creates an ordered, hash-verified catalogue.
2. `resolve` selects a bounded skill and command dependency closure.
3. `read --many` hydrates those bodies in one process.
4. `doctor` checks every path, dependency and body hash.

The ordered index uses Rust's B-tree maps for reproducible output. The speed comes from doing less work and reading selected files together, not from claiming that a B-tree makes Markdown parsing magically faster.

## Measured result

We tested Skillleaf against one real local library with 382 entries and 3.34 MB of Markdown. A representative pull request review selected 101 KB, leaving about 3.24 MB out of the prompt. Using a rough four-bytes-per-token estimate, that avoided approximately 810,000 input tokens for that request.

Across 20 warm runs on an Apple Silicon development machine, resolution averaged 4.8 ms and hydration of eight bodies averaged 4.3 ms. Rebuilding the full catalogue averaged 96.6 ms across 10 runs.

Those numbers describe one machine, library and task. Measure your own setup before making capacity or cost claims.

## Quick start

```sh
cargo install --path .

skillleaf index \
  --skills personal="$HOME/.claude/skills" \
  --commands personal="$HOME/.claude/commands" \
  --output "$HOME/.config/skillleaf/catalog.json"

export SKILLLEAF_CATALOG="$HOME/.config/skillleaf/catalog.json"
export SKILLLEAF_USAGE_FILE="$HOME/.config/skillleaf/usage.json"

skillleaf resolve --task "review and finish this code change"
skillleaf read --many personal/skill:critical-review
skillleaf stats --format text
skillleaf doctor
```

The repository also includes a compact host-neutral router skill at [`skills/skillleaf/SKILL.md`](skills/skillleaf/SKILL.md). Install or link that one skill in your agent host. Keep the larger skill library outside the prompt.

## Install

Build from a checked-out copy:

```sh
cargo install --path . --locked
skillleaf --help
```

After the repository is public, Cargo can install the same source directly:

```sh
cargo install --git https://github.com/derek-codebridge/skillleaf --locked
```

Skillleaf does not change Claude Code, Codex, OpenCode or your skill folders during installation. That is deliberate. Migration stays visible and reversible.

## Migrate an existing library safely

Do not delete your current skills or commands. Back them up, copy them into a host-neutral library, verify the copy, then remove the originals from the host's discovery path only after a fresh session proves the router works.

### macOS and Linux

This example uses Claude Code folders. Repeat the copy for any other host-specific library you want Skillleaf to route.

```sh
skillleaf_root="$HOME/.local/share/skillleaf"
backup_root="$HOME/.local/share/skillleaf-backups/$(date +%Y%m%d-%H%M%S)"

mkdir -p "$skillleaf_root/library/claude-skills" \
  "$skillleaf_root/library/claude-commands" \
  "$backup_root/claude-skills" \
  "$backup_root/claude-commands"
cp -R "$HOME/.claude/skills/." "$backup_root/claude-skills/"
cp -R "$HOME/.claude/commands/." "$backup_root/claude-commands/"
cp -R "$HOME/.claude/skills/." "$skillleaf_root/library/claude-skills/"
cp -R "$HOME/.claude/commands/." "$skillleaf_root/library/claude-commands/"
tar -czf "$backup_root/claude-library.tar.gz" \
  -C "$backup_root" claude-skills claude-commands

skillleaf index \
  --skills personal="$skillleaf_root/library/claude-skills" \
  --commands personal="$skillleaf_root/library/claude-commands" \
  --output "$skillleaf_root/catalog.json"

SKILLLEAF_CATALOG="$skillleaf_root/catalog.json" skillleaf doctor
```

Run the command-line verification below before changing the host. For cutover, move the original active skill directory to a recoverable name outside the host's discovery path, create a new empty active directory, then copy `skills/skillleaf/SKILL.md` into a `skillleaf` folder beneath it. Start a clean agent session and confirm it follows the router. Restore the renamed directory immediately if it does not.

Host-native slash commands are a separate boundary. Moving a command file out of the host's discovery path may remove that `/command` from the host interface. Leave commands that must remain directly invokable in their native directory; Skillleaf can route their instruction bodies, but it does not register host UI commands.

### Windows PowerShell

```powershell
$SkillleafRoot = Join-Path $env:LOCALAPPDATA "Skillleaf"
$BackupRoot = Join-Path $SkillleafRoot ("backups\" + (Get-Date -Format "yyyyMMdd-HHmmss"))
New-Item -ItemType Directory -Force "$SkillleafRoot\library", $BackupRoot | Out-Null
Copy-Item -Recurse -Force "$HOME\.claude\skills" "$BackupRoot\claude-skills"
Copy-Item -Recurse -Force "$HOME\.claude\commands" "$BackupRoot\claude-commands"
Copy-Item -Recurse -Force "$HOME\.claude\skills" "$SkillleafRoot\library\claude-skills"
Copy-Item -Recurse -Force "$HOME\.claude\commands" "$SkillleafRoot\library\claude-commands"
Compress-Archive -Force \
  -Path "$BackupRoot\claude-skills", "$BackupRoot\claude-commands" \
  -DestinationPath "$BackupRoot\claude-library.zip"

skillleaf index `
  --skills "personal=$SkillleafRoot\library\claude-skills" `
  --commands "personal=$SkillleafRoot\library\claude-commands" `
  --output "$SkillleafRoot\catalog.json"

$env:SKILLLEAF_CATALOG = "$SkillleafRoot\catalog.json"
$env:SKILLLEAF_USAGE_FILE = "$SkillleafRoot\usage.json"
skillleaf doctor
```

Persist the two environment variables using your shell, agent launcher or user environment settings. Avoid placing them in a repository that other people can clone.

## Verify before switching over

Use a task that should select a known skill:

```sh
skillleaf resolve --task "review and finish this code change"
skillleaf read --many personal/skill:critical-review
skillleaf stats --format text
skillleaf doctor
```

Confirm five things before moving the original folders out of the host discovery path:

1. `resolve` returns the expected skill rather than an unrelated name match.
2. Linked Markdown references appear in the selected dependency closure.
3. `read --many` returns every selected body in one process.
4. `doctor` passes after the source folders are copied to their final location.
5. A clean agent session follows the router skill without preloading the old library.

Rollback is simply restoring the backed-up folders to the host's discovery path and starting a new session. The catalogue and usage ledger can remain on disk because the host does not read them without the adapter.

## Expected benefit

Skillleaf helps when the host would otherwise preload a large skill and command library. In that shape, users should see:

- smaller session-start prompts because only the router description is always visible;
- lower input-token use because irrelevant bodies stay on disk;
- faster selected-body loading because one process hydrates the complete dependency closure;
- deterministic selection and output, without an embedding service or model call;
- verified nested Markdown dependencies, loaded only when their parent skill is selected;
- one catalogue shared across Claude Code, Codex, OpenCode, CI and custom agent runners;
- local usage evidence showing which entries earn maintenance and which never get hydrated.

The host must honour the router workflow for these savings to appear. Skillleaf cannot stop a host from preloading files that remain in its own discovery directory. Measure a clean session before and after migration rather than assuming the result.

## Supported layout

Skill roots contain folders with a `SKILL.md`. Markdown files beneath the same folder become separately addressable resources. Relative Markdown links from `SKILL.md` become verified dependencies.

Command roots contain Markdown files. Optional frontmatter supports a name, description and explicit dependencies:

```yaml
---
name: finish
description: Finish a change with review and verification.
dependencies:
  - personal/skill:critical-review
---
```

Skillleaf deliberately supports this small frontmatter surface. The parser is line-based, deterministic and tolerant of common human-written descriptions that strict YAML parsers reject.

Relative Markdown links and explicit `dependencies` form the verified chain. Arbitrary `@file` references are not parsed in version 0.1.0; convert important ones to Markdown links or explicit selectors so missing references fail during indexing.

## Security boundaries

- Catalogue and body SHA-256 hashes fail closed.
- Hydration rejects path traversal, root escape, symlinks, non-regular files and changed bodies.
- Inputs are size-bounded UTF-8 Markdown.
- Catalogue writes use atomic temporary-file replacement.
- Source collisions and missing dependencies are errors.
- Prompts, code and credentials stay on the computer.
- Optional usage counts store selectors, hashes, counts and timestamps locally. They never store prompts or task text.
- Skillleaf reads instructions. It does not execute bundled scripts.

## Deliberate limits

Skillleaf does not install skills, modify agent settings or infer private workflow policy. It does not use fuzzy matching, embeddings or an LLM. Deterministic lexical routing is the dependable first path; future rerankers can sit above it without weakening hashes, containment or explicit dependencies.

## Use it with an agent

An agent host needs one compact instruction:

```text
Before loading skills, run skillleaf resolve for the task. Read the returned
selectors together with skillleaf read --many. Do not preload unselected bodies.
```

Claude Code, Codex, OpenCode, CI and custom agent runners can use the same binary. Host-specific installers can remain separate from the verified core.

## See what earns its place

Set `SKILLLEAF_USAGE_FILE` to enable local hydration counts. `skillleaf stats` lists used and never-hydrated skills and commands, so you can remove dead entries, sharpen weak descriptions and focus maintenance where agents actually work.

Counting happens only after a body passes hash and containment checks. Concurrent agent processes share a file lock and atomic write path, so increments are not silently lost. The feature is opt-in and does not send data anywhere.

## Licence

Skillleaf is source-available under the [MIT License with the Commons Clause License Condition v1.0](LICENSE). It is not OSI open source because selling substantially the same software is restricted.

Individuals and businesses may use, modify and run Skillleaf, including for internal commercial work. Selling Skillleaf itself, a lightly modified substitute, or a paid service whose value derives substantially from Skillleaf requires a [separate commercial licence](COMMERCIAL-LICENCE.md).

Contributions are welcome under the [contribution terms](CONTRIBUTOR-LICENCE-AGREEMENT.md). Names and branding remain subject to the [trademark policy](TRADEMARKS.md).
