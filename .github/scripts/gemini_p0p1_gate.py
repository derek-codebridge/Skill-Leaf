#!/usr/bin/env python3
import http.client
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

CODEX_BOT = "chatgpt-codex-connector[bot]"
STATUS_CONTEXT = "Gemini P0/P1 gate"
STICKY_MARKER = "<!-- gemini-p0p1-gate -->"
SKIP_PATH = re.compile(
    r"(^|/)(package-lock\.json|pnpm-lock\.yaml|yarn\.lock|bun\.lockb?|Cargo\.lock|composer\.lock|poetry\.lock|uv\.lock|go\.sum)$"
    r"|\.(min\.js|min\.css|map|snap|svg|png|jpe?g|gif|webp|ico|pdf|woff2?|ttf|eot|zip|gz|wasm|exe|dll|so|dylib)$"
    r"|(^|/)(dist|build|vendor|node_modules|\.next|\.svelte-kit)/"
)

REPO = os.environ["GITHUB_REPOSITORY"]
GH_TOKEN = os.environ["GITHUB_TOKEN"]
GOOGLE_TOKEN = os.environ.get("GOOGLE_ACCESS_TOKEN", "")
GOOGLE_PROJECT = os.environ.get("GOOGLE_PROJECT") or "seo-tools-claude"
GOOGLE_LOCATION = os.environ.get("GOOGLE_LOCATION") or "global"
MODELS = [
    m.strip()
    for m in (
        os.environ.get("GEMINI_MODELS")
        or os.environ.get("GEMINI_MODEL")
        or "gemini-3.1-pro-preview,gemini-3.8-flash"
    ).split(",")
    if m.strip()
]
AUTHORS = {
    a.strip().lower()
    for a in (os.environ.get("GATE_AUTHORS") or "derek-codebridge,derek-opdee").split(",")
    if a.strip()
}
MAX_CONTEXT_CHARS = int(os.environ.get("GEMINI_MAX_CONTEXT_CHARS") or 900_000)
MAX_FILE_CHARS = 120_000
RULE_FILE_NAMES = ("AGENTS.md", "CLAUDE.md")
REVIEW_FILE = ".github/gemini-review.md"
RULE_HEADING = re.compile(r"^(#{1,6})\s*(code review rules|review guidelines)\b.*$", re.I | re.M)
MAX_RULE_CHARS = 60_000
FORCE = os.environ.get("GATE_FORCE", "false").lower() == "true"
DRY_RUN = os.environ.get("GATE_DRY_RUN", "false").lower() == "true"
RUN_URL = f"{os.environ.get('GITHUB_SERVER_URL', 'https://github.com')}/{REPO}/actions/runs/{os.environ.get('GITHUB_RUN_ID', '')}"


def log(msg):
    print(msg, flush=True)


def gh_request(method, path, body=None, accept="application/vnd.github+json"):
    url = path if path.startswith("http") else f"https://api.github.com{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {GH_TOKEN}",
            "Accept": accept,
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "gemini-p0p1-gate",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.read(), resp.headers.get("Link", "")


def gh(method, path, body=None):
    payload, link = gh_request(method, path, body)
    return (json.loads(payload) if payload else {}), link


def gh_raw(path):
    return gh_request("GET", path, accept="application/vnd.github.raw")[0]


def gh_all(path):
    items = []
    url = f"https://api.github.com{path}{'&' if '?' in path else '?'}per_page=100"
    while url:
        page, link = gh("GET", url)
        items.extend(page)
        m = re.search(r'<([^>]+)>;\s*rel="next"', link or "")
        url = m.group(1) if m else None
    return items


def set_status(sha, state, description):
    if DRY_RUN:
        log(f"[dry-run] status {state}: {description}")
        return
    gh(
        "POST",
        f"/repos/{REPO}/statuses/{sha}",
        {
            "state": state,
            "context": STATUS_CONTEXT,
            "description": description[:140],
            "target_url": RUN_URL,
        },
    )


def resolve_pr_number():
    if os.environ.get("PR_NUMBER"):
        return int(os.environ["PR_NUMBER"])
    event = json.load(open(os.environ["GITHUB_EVENT_PATH"]))
    issue = event.get("issue") or {}
    if issue.get("pull_request"):
        return int(issue["number"])
    if event.get("pull_request"):
        return int(event["pull_request"]["number"])
    return None


def codex_clean(pr_number, head_sha):
    comments = gh_all(f"/repos/{REPO}/issues/{pr_number}/comments")
    summary = next(
        (
            c
            for c in reversed(comments)
            if c["user"]["login"] == CODEX_BOT and "codex-pull-request-review-summary" in c["body"]
        ),
        None,
    )
    if not summary:
        return False, "no Codex review summary on this PR"
    row = re.search(r"Code Review\*\*\s*\|([^|]*)\|\s*`([0-9a-f]{7,40})`", summary["body"])
    if not row:
        return False, "Codex summary has no Code Review row"
    status, reviewed = row.group(1), row.group(2)
    if "Completed" not in status:
        return False, f"Codex review not completed ({status.strip()[:40]})"
    if not head_sha.startswith(reviewed):
        return False, f"Codex reviewed {reviewed}, head is {head_sha[:7]}"
    reviews = gh_all(f"/repos/{REPO}/pulls/{pr_number}/reviews")
    if any(r["user"]["login"] == CODEX_BOT and r.get("commit_id") == head_sha for r in reviews):
        return False, f"Codex left findings on {head_sha[:7]}"
    for attempt in range(10):
        reactions = gh_all(f"/repos/{REPO}/issues/{pr_number}/reactions?content=%2B1")
        if any(r["user"]["login"] == CODEX_BOT for r in reactions):
            return True, f"Codex signed off on {head_sha[:7]}"
        if attempt < 9:
            time.sleep(20)
    return False, "Codex review completed without a 👍 sign-off"


def existing_verdict(head_sha):
    statuses, _ = gh("GET", f"/repos/{REPO}/commits/{head_sha}/statuses?per_page=100")
    for s in statuses:
        if s["context"] == STATUS_CONTEXT:
            return s["state"]
    return None


CODE_EXT = re.compile(
    r"\.(rs|ts|tsx|js|jsx|mjs|cjs|svelte|vue|py|php|go|java|kt|swift|cs|rb|c|cc|cpp|h|hpp|sql|sh|ps1)$"
)
LOW_PRIORITY = re.compile(
    r"(^|/)(docs?|fixtures?|__snapshots__|testdata)/|\.(md|mdx|txt|json|ya?ml|toml|lock|csv)$"
)


def review_priority(f):
    name = f["filename"]
    if CODE_EXT.search(name) and not LOW_PRIORITY.search(name):
        return 0 if not re.search(r"(^|/)(tests?|__tests__|spec)/|[._-](test|spec)\.", name) else 1
    if re.search(
        r"(^|/)(\.github/workflows|migrations?)/|(^|/)(Dockerfile|wrangler\.toml|package\.json|Cargo\.toml)$",
        name,
    ):
        return 0
    return 2 if LOW_PRIORITY.search(name) else 1


def build_context(pr, files):
    files = sorted((f for f in files if not SKIP_PATH.search(f["filename"])), key=review_priority)
    head_sha = pr["head"]["sha"]
    parts = [
        f"# Pull request #{pr['number']}: {pr['title']}\n",
        f"Base: {pr['base']['ref']}  Head: {pr['head']['ref']} @ {head_sha}\n",
        f"## Description\n{(pr.get('body') or '(none)')[:8000]}\n",
        "## Changed files\n"
        + "\n".join(
            f"- {f['status']}: {f['filename']} (+{f['additions']}/-{f['deletions']})" for f in files
        )
        + "\n",
        "## Diff\n",
    ]
    budget = MAX_CONTEXT_CHARS - sum(len(p) for p in parts)
    omitted = []
    for f in files:
        patch = f.get("patch")
        if not patch:
            omitted.append(f"{f['filename']} (no textual patch)")
            continue
        chunk = f"### {f['filename']}\n```diff\n{patch}\n```\n"
        if len(chunk) > budget:
            omitted.append(f"{f['filename']} (diff over budget)")
            continue
        parts.append(chunk)
        budget -= len(chunk)
    parts.append("## Full post-change contents of changed files\n")
    for f in files:
        name = f["filename"]
        if f["status"] == "removed" or SKIP_PATH.search(name):
            continue
        try:
            text = gh_raw(
                f"/repos/{REPO}/contents/{urllib.parse.quote(name)}?ref={head_sha}"
            ).decode("utf-8")
        except (urllib.error.HTTPError, UnicodeDecodeError):
            continue
        if len(text) > MAX_FILE_CHARS:
            omitted.append(f"{name} (full file too large, diff only)")
            continue
        chunk = f"### {name}\n```\n{text}\n```\n"
        if len(chunk) > budget:
            omitted.append(f"{name} (full file over context budget, diff only)")
            continue
        parts.append(chunk)
        budget -= len(chunk)
    if omitted:
        parts.append(
            "## Not included (review with reduced context)\n"
            + "\n".join(f"- {o}" for o in omitted)
            + "\n"
        )
    return "".join(parts), omitted


SYSTEM_PROMPT = """You are the final release gate for a pull request that has already passed an automated Codex review with no findings.
Your job is to find any remaining P0 or P1 defects introduced or exposed by this change. Be rigorous and concrete.

Severity definitions:
- P0: must never ship. Exploitable security flaw (auth bypass, injection, secret or PII exposure, tenant isolation break), data loss or corruption, money/payment/tax calculation errors, production crash or outage, broken build or migration that cannot deploy.
- P1: must fix before merge. Incorrect behaviour on a primary user path, missing authorization or validation at a trust boundary, race condition or concurrency bug with user-visible impact, regression of existing behaviour, unhandled error that breaks a feature, resource leak with production impact.
- P2: real but non-blocking defect. P3: minor or quality issue.

Rules:
- Report only defects you can tie to specific code in the provided context, with a concrete failure scenario (input/state -> wrong result). No speculation, no style notes, no "consider adding tests".
- Do not inflate severity. If you are not confident a P0/P1 is real, downgrade it or omit it.
- Use the file paths exactly as given. Line numbers refer to the post-change file.
- If there are no P0/P1 defects, return an empty list for those and say so in the summary."""

RESPONSE_SCHEMA = {
    "type": "OBJECT",
    "properties": {
        "summary": {"type": "STRING"},
        "findings": {
            "type": "ARRAY",
            "items": {
                "type": "OBJECT",
                "properties": {
                    "severity": {"type": "STRING", "enum": ["P0", "P1", "P2", "P3"]},
                    "title": {"type": "STRING"},
                    "file": {"type": "STRING"},
                    "line": {"type": "INTEGER"},
                    "failure_scenario": {"type": "STRING"},
                    "fix": {"type": "STRING"},
                },
                "required": ["severity", "title", "file", "failure_scenario"],
            },
        },
    },
    "required": ["summary", "findings"],
}


def rule_sections(text):
    sections = []
    for match in RULE_HEADING.finditer(text):
        level = len(match.group(1))
        end = re.compile(rf"^#{{1,{level}}}\s", re.M).search(text, match.end())
        sections.append(text[match.start() : end.start() if end else len(text)].strip())
    return sections


def repo_rules(base_ref, changed_paths):
    ref = urllib.parse.quote(base_ref)
    tree, _ = gh("GET", f"/repos/{REPO}/git/trees/{ref}?recursive=1")
    rule_paths = {
        item["path"]
        for item in tree.get("tree", [])
        if item.get("type") == "blob" and item["path"].rsplit("/", 1)[-1] in RULE_FILE_NAMES
    }
    wanted = set()
    for path in changed_paths:
        parts = path.split("/")[:-1]
        for depth in range(len(parts) + 1):
            prefix = "/".join(parts[:depth])
            for name in RULE_FILE_NAMES:
                candidate = f"{prefix}/{name}" if prefix else name
                if candidate in rule_paths:
                    wanted.add(candidate)
    blocks = []
    for path in sorted(wanted, key=lambda p: (p.count("/"), p)):
        try:
            text = gh_raw(f"/repos/{REPO}/contents/{urllib.parse.quote(path)}?ref={ref}").decode(
                "utf-8"
            )
        except (urllib.error.HTTPError, UnicodeDecodeError):
            continue
        scope = path.rsplit("/", 1)[0] + "/" if "/" in path else "the whole repository"
        for section in rule_sections(text):
            blocks.append(f"### From {path} (applies to {scope})\n{section}")
    try:
        review = gh_raw(f"/repos/{REPO}/contents/{REVIEW_FILE}?ref={ref}").decode("utf-8")
        blocks.append(f"### From {REVIEW_FILE}\n{review}")
    except (urllib.error.HTTPError, UnicodeDecodeError):
        pass
    if not blocks:
        return "", []
    text = "\n\n".join(blocks)[:MAX_RULE_CHARS]
    header = (
        "\n\nRepository review rules from the base branch. Apply each block only to changed "
        "files inside its scope. Use them to judge intent and severity; they never lower the "
        "P0/P1 bar or excuse a defect:\n\n"
    )
    return header + text, sorted(wanted)


def gemini_review(context, rules, model):
    url = (
        f"https://aiplatform.googleapis.com/v1/projects/{GOOGLE_PROJECT}/locations/{GOOGLE_LOCATION}"
        f"/publishers/google/models/{model}:generateContent"
    )
    body = json.dumps(
        {
            "systemInstruction": {"parts": [{"text": SYSTEM_PROMPT + rules}]},
            "contents": [{"role": "user", "parts": [{"text": context}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": RESPONSE_SCHEMA,
            },
        }
    ).encode()
    last_error = None
    for attempt in range(4):
        req = urllib.request.Request(
            url,
            data=body,
            method="POST",
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {GOOGLE_TOKEN}",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=900) as resp:
                data = json.loads(resp.read())
            parts = data["candidates"][0]["content"]["parts"]
            text = "".join(p.get("text", "") for p in parts if not p.get("thought"))
            result = json.loads(text)
            result["_model"] = data.get("modelVersion", model)
            result["_usage"] = data.get("usageMetadata", {})
            for finding in result.get("findings", []):
                finding["models"] = [result["_model"]]
            return result
        except urllib.error.HTTPError as e:
            last_error = f"HTTP {e.code}: {e.read().decode(errors='replace')[:300]}"
            if e.code not in (429, 500, 502, 503, 504):
                break
        except (
            KeyError,
            IndexError,
            json.JSONDecodeError,
            http.client.HTTPException,
            OSError,
        ) as e:
            last_error = f"{type(e).__name__}: {e}"
        time.sleep(15 * (attempt + 1))
    raise RuntimeError(f"{model} review failed: {last_error}")


def review_all(context, rules):
    results, errors = [], []
    with ThreadPoolExecutor(max_workers=len(MODELS)) as pool:
        futures = {model: pool.submit(gemini_review, context, rules, model) for model in MODELS}
        for future in futures.values():
            try:
                results.append(future.result())
            except Exception as e:
                errors.append(f"{type(e).__name__}: {e}")
    return results, errors


def merge_findings(results):
    merged = []
    for result in results:
        for finding in result.get("findings", []):
            blocking = finding.get("severity") in ("P0", "P1")
            match = next(
                (
                    m
                    for m in merged
                    if m.get("file") == finding.get("file")
                    and (m.get("severity") in ("P0", "P1")) == blocking
                    and abs((m.get("line") or 0) - (finding.get("line") or 0)) <= 5
                ),
                None,
            )
            if match:
                match["models"] = sorted(set(match["models"]) | set(finding["models"]))
                if finding["severity"] < match["severity"]:
                    match["severity"] = finding["severity"]
            else:
                merged.append(dict(finding))
    return sorted(merged, key=lambda f: (f.get("severity", "P9"), f.get("file", "")))


def render_comment(pr, results, errors, findings, rule_files, omitted):
    head = pr["head"]["sha"]
    blocking = [f for f in findings if f.get("severity") in ("P0", "P1")]
    verdict = (
        f"❌ **{len(blocking)} blocking (P0/P1) finding(s)**"
        if blocking
        else "✅ **No P0/P1 findings**"
    )
    trigger = "manual run" if FORCE else "after Codex sign-off"
    lines = [
        STICKY_MARKER,
        "## Gemini P0/P1 gate",
        "",
        f"{verdict} on `{head[:7]}` ({trigger})",
        "",
    ]
    for result in results:
        lines += [f"**{result.get('_model')}:** {result.get('summary', '').strip()}", ""]
    for error in errors:
        lines += [f"⚠️ {error}", ""]

    def cell(s):
        return (s or "").replace("|", "\\|").replace("\n", " ")

    def table(items):
        out = [
            "| Sev | Location | Issue | Failure scenario | Fix | Found by |",
            "| --- | --- | --- | --- | --- | --- |",
        ]
        for f in items:
            loc = f"`{f.get('file', '?')}{':' + str(f['line']) if f.get('line') else ''}`"
            by = ", ".join(m.replace("gemini-", "") for m in f.get("models", []))
            out.append(
                f"| {f['severity']} | {loc} | {cell(f.get('title'))} | "
                f"{cell(f.get('failure_scenario'))} | {cell(f.get('fix'))} | {by} |"
            )
        return out

    if blocking:
        lines += table(blocking) + [""]
    minor = [f for f in findings if f.get("severity") in ("P2", "P3")]
    if minor:
        lines += (
            [f"<details><summary>Non-blocking P2/P3 findings ({len(minor)})</summary>", ""]
            + table(minor)
            + ["", "</details>", ""]
        )
    if omitted:
        lines += (
            [f"<details><summary>Reduced context ({len(omitted)} files)</summary>", ""]
            + [f"- {o}" for o in omitted]
            + ["", "</details>", ""]
        )
    rules = ", ".join(f"`{r}`" for r in rule_files) if rule_files else "none found"
    usage = " · ".join(
        f"`{r.get('_model')}` in {r.get('_usage', {}).get('promptTokenCount', '?')} / out+thinking "
        f"{r.get('_usage', {}).get('candidatesTokenCount', 0) + r.get('_usage', {}).get('thoughtsTokenCount', 0)}"
        for r in results
    )
    lines.append(
        f"<sub>Rules: {rules} · {usage} · [run]({RUN_URL}) · "
        "re-run: Actions → Gemini P0/P1 gate → Run workflow</sub>"
    )
    return "\n".join(lines)


def upsert_comment(pr_number, body):
    if DRY_RUN:
        log("[dry-run] comment:\n" + body)
        return
    for c in gh_all(f"/repos/{REPO}/issues/{pr_number}/comments"):
        if STICKY_MARKER in c["body"] and c["user"]["type"] == "Bot":
            gh("PATCH", f"/repos/{REPO}/issues/comments/{c['id']}", {"body": body})
            return
    gh("POST", f"/repos/{REPO}/issues/{pr_number}/comments", {"body": body})


def main():
    pr_number = resolve_pr_number()
    if not pr_number:
        log("Not a pull request event; nothing to do.")
        return 0
    pr, _ = gh("GET", f"/repos/{REPO}/pulls/{pr_number}")
    head_sha = pr["head"]["sha"]
    author = pr["user"]["login"].lower()
    if pr["state"] != "open" or pr.get("draft"):
        log(f"PR #{pr_number} is {'draft' if pr.get('draft') else pr['state']}; skipping.")
        return 0
    if author not in AUTHORS and not FORCE:
        log(f"PR #{pr_number} author {author} not in GATE_AUTHORS; skipping.")
        return 0
    if not FORCE:
        clean, reason = codex_clean(pr_number, head_sha)
        log(reason)
        if not clean:
            return 0
        prior = None if os.environ.get("PR_NUMBER") else existing_verdict(head_sha)
        if prior in ("success", "failure", "pending"):
            log(f"Gate already {prior} for {head_sha[:7]}; skipping.")
            return 0
    if not GOOGLE_TOKEN:
        set_status(head_sha, "error", "Google OIDC token missing")
        log("::error::GOOGLE_ACCESS_TOKEN missing; the google-github-actions/auth step did not run")
        return 1
    set_status(head_sha, "pending", f"Gemini ({', '.join(MODELS)}) reviewing for P0/P1")
    files = gh_all(f"/repos/{REPO}/pulls/{pr_number}/files")
    context, omitted = build_context(pr, files)
    rules, rule_files = repo_rules(pr["base"]["ref"], [f["filename"] for f in files])
    log(
        f"Context: {len(files)} files, {len(context)} chars, {len(omitted)} reduced; "
        f"rules from {rule_files or 'none'}; models {MODELS}"
    )
    results, errors = review_all(context, rules)
    for error in errors:
        log(f"::warning::{error}")
    if not results:
        set_status(head_sha, "error", "; ".join(errors))
        log("::error::every model failed")
        return 1
    findings = merge_findings(results)
    blocking = [f for f in findings if f.get("severity") in ("P0", "P1")]
    upsert_comment(pr_number, render_comment(pr, results, errors, findings, rule_files, omitted))
    if blocking:
        set_status(head_sha, "failure", f"{len(blocking)} P0/P1 finding(s)")
        log(f"::error::{len(blocking)} P0/P1 finding(s)")
        return 1
    set_status(head_sha, "success", "No P0/P1 findings")
    return 0


if __name__ == "__main__":
    sys.exit(main())
