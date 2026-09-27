#!/usr/bin/env python3
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

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
MODEL = os.environ.get("GEMINI_MODEL") or "gemini-3.1-pro-preview"
AUTHORS = {a.strip().lower() for a in (os.environ.get("GATE_AUTHORS") or "derek-codebridge,derek-opdee").split(",") if a.strip()}
MAX_CONTEXT_CHARS = int(os.environ.get("GEMINI_MAX_CONTEXT_CHARS") or 900_000)
MAX_FILE_CHARS = 120_000
FORCE = os.environ.get("GATE_FORCE", "false").lower() == "true"
DRY_RUN = os.environ.get("GATE_DRY_RUN", "false").lower() == "true"
RUN_URL = f"{os.environ.get('GITHUB_SERVER_URL', 'https://github.com')}/{REPO}/actions/runs/{os.environ.get('GITHUB_RUN_ID', '')}"


def log(msg):
    print(msg, flush=True)


def gh_request(method, path, body=None, accept="application/vnd.github+json"):
    url = path if path.startswith("http") else f"https://api.github.com{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method, headers={
        "Authorization": f"Bearer {GH_TOKEN}",
        "Accept": accept,
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "gemini-p0p1-gate",
    })
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
    gh("POST", f"/repos/{REPO}/statuses/{sha}", {
        "state": state,
        "context": STATUS_CONTEXT,
        "description": description[:140],
        "target_url": RUN_URL,
    })


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
    summary = next((c for c in reversed(comments)
                    if c["user"]["login"] == CODEX_BOT and "codex-pull-request-review-summary" in c["body"]), None)
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


CODE_EXT = re.compile(r"\.(rs|ts|tsx|js|jsx|mjs|cjs|svelte|vue|py|php|go|java|kt|swift|cs|rb|c|cc|cpp|h|hpp|sql|sh|ps1)$")
LOW_PRIORITY = re.compile(r"(^|/)(docs?|fixtures?|__snapshots__|testdata)/|\.(md|mdx|txt|json|ya?ml|toml|lock|csv)$")


def review_priority(f):
    name = f["filename"]
    if CODE_EXT.search(name) and not LOW_PRIORITY.search(name):
        return 0 if not re.search(r"(^|/)(tests?|__tests__|spec)/|[._-](test|spec)\.", name) else 1
    if re.search(r"(^|/)(\.github/workflows|migrations?)/|(^|/)(Dockerfile|wrangler\.toml|package\.json|Cargo\.toml)$", name):
        return 0
    return 2 if LOW_PRIORITY.search(name) else 1


def build_context(pr, files):
    files = sorted((f for f in files if not SKIP_PATH.search(f["filename"])), key=review_priority)
    head_sha = pr["head"]["sha"]
    parts = [
        f"# Pull request #{pr['number']}: {pr['title']}\n",
        f"Base: {pr['base']['ref']}  Head: {pr['head']['ref']} @ {head_sha}\n",
        f"## Description\n{(pr.get('body') or '(none)')[:8000]}\n",
        "## Changed files\n" + "\n".join(f"- {f['status']}: {f['filename']} (+{f['additions']}/-{f['deletions']})" for f in files) + "\n",
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
            text = gh_raw(f"/repos/{REPO}/contents/{urllib.parse.quote(name)}?ref={head_sha}").decode("utf-8")
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
        parts.append("## Not included (review with reduced context)\n" + "\n".join(f"- {o}" for o in omitted) + "\n")
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


def gemini_review(context):
    url = (f"https://aiplatform.googleapis.com/v1/projects/{GOOGLE_PROJECT}/locations/{GOOGLE_LOCATION}"
           f"/publishers/google/models/{MODEL}:generateContent")
    body = json.dumps({
        "systemInstruction": {"parts": [{"text": SYSTEM_PROMPT}]},
        "contents": [{"role": "user", "parts": [{"text": context}]}],
        "generationConfig": {"responseMimeType": "application/json", "responseSchema": RESPONSE_SCHEMA},
    }).encode()
    last_error = None
    for attempt in range(4):
        req = urllib.request.Request(url, data=body, method="POST", headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {GOOGLE_TOKEN}",
        })
        try:
            with urllib.request.urlopen(req, timeout=900) as resp:
                data = json.loads(resp.read())
            parts = data["candidates"][0]["content"]["parts"]
            text = "".join(p.get("text", "") for p in parts if not p.get("thought"))
            result = json.loads(text)
            result["_model"] = data.get("modelVersion", MODEL)
            result["_usage"] = data.get("usageMetadata", {})
            return result
        except urllib.error.HTTPError as e:
            last_error = f"HTTP {e.code}: {e.read().decode(errors='replace')[:300]}"
            if e.code not in (429, 500, 502, 503, 504):
                break
        except (KeyError, IndexError, json.JSONDecodeError, TimeoutError, urllib.error.URLError) as e:
            last_error = f"{type(e).__name__}: {e}"
        time.sleep(15 * (attempt + 1))
    raise RuntimeError(f"Gemini review failed: {last_error}")


def render_comment(pr, result, blocking, omitted):
    head = pr["head"]["sha"]
    usage = result.get("_usage", {})
    verdict = f"❌ **{len(blocking)} blocking (P0/P1) finding(s)**" if blocking else "✅ **No P0/P1 findings**"
    lines = [STICKY_MARKER, "## Gemini P0/P1 gate", "",
             f"{verdict} on `{head[:7]}` ({'manual run' if FORCE else 'after Codex sign-off'})", "",
             result.get("summary", "").strip(), ""]
    def table(items):
        out = ["| Sev | Location | Issue | Failure scenario | Fix |", "| --- | --- | --- | --- | --- |"]
        for f in items:
            loc = f"`{f.get('file', '?')}{':' + str(f['line']) if f.get('line') else ''}`"
            cell = lambda s: (s or "").replace("|", "\\|").replace("\n", " ")
            out.append(f"| {f['severity']} | {loc} | {cell(f.get('title'))} | {cell(f.get('failure_scenario'))} | {cell(f.get('fix'))} |")
        return out
    if blocking:
        lines += table(blocking) + [""]
    minor = [f for f in result.get("findings", []) if f.get("severity") in ("P2", "P3")]
    if minor:
        lines += ["<details><summary>Non-blocking P2/P3 findings ({})</summary>".format(len(minor)), ""] + table(minor) + ["", "</details>", ""]
    if omitted:
        lines += ["<details><summary>Reduced context ({} files)</summary>".format(len(omitted)), ""] + [f"- {o}" for o in omitted] + ["", "</details>", ""]
    lines.append(f"<sub>Model `{result.get('_model')}` · input tokens {usage.get('promptTokenCount', '?')} · [run]({RUN_URL}) · re-run: Actions → Gemini P0/P1 gate → Run workflow</sub>")
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
    set_status(head_sha, "pending", f"Gemini ({MODEL}) reviewing for P0/P1")
    files = gh_all(f"/repos/{REPO}/pulls/{pr_number}/files")
    context, omitted = build_context(pr, files)
    log(f"Context: {len(files)} files, {len(context)} chars, {len(omitted)} reduced")
    try:
        result = gemini_review(context)
    except RuntimeError as e:
        set_status(head_sha, "error", str(e))
        log(f"::error::{e}")
        return 1
    blocking = [f for f in result.get("findings", []) if f.get("severity") in ("P0", "P1")]
    upsert_comment(pr_number, render_comment(pr, result, blocking, omitted))
    if blocking:
        set_status(head_sha, "failure", f"{len(blocking)} P0/P1 finding(s)")
        log(f"::error::{len(blocking)} P0/P1 finding(s)")
        return 1
    set_status(head_sha, "success", "No P0/P1 findings")
    return 0


if __name__ == "__main__":
    sys.exit(main())
