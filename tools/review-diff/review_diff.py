#!/usr/bin/env python3
"""review-diff: review a repo's tested commits with your own AI accounts.

The home mesh's code review, for a machine that is not on the mesh (the work
laptop): no mac1, no pi1, nothing sent anywhere but the AI accounts named in
the config. Same prompts (codereview/prompts in this repo), same five kinds
of item, same report: an explanation and a diff for each, for Claude Code to
fix or drop. docs/review-diff.md says how to set it up.

    review_diff.py init                 write an example config
    review_diff.py run [--upto REV]     review the commits since the last run, up to REV
    review_diff.py mark ID STATUS       record a finding as fixed, dismissed or open
    review_diff.py score                each reviewer's record

Standard library only, so it runs wherever Python 3.9+ and git do.
"""
from __future__ import annotations

import argparse
import base64
import datetime as dt
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_PROMPTS = HERE.parent.parent / "codereview" / "prompts"
DEFAULT_CONFIG = Path.home() / ".config" / "review-diff" / "config.json"

# What gets reviewed. The home list plus C and C++, for firmware.
REVIEWABLE_EXTS = {
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "swift", "kt", "java", "rb",
    "php", "cs", "sql", "sh", "ps1", "rules", "vue", "svelte", "c", "h", "cc", "cpp", "hpp",
    "cxx", "hxx",
}
SKIP_DIRS = {
    "node_modules", "dist", "build", "vendor", "target", "coverage", ".git", "Pods", ".next",
    ".expo", "__generated__", "third_party", "external",
}
MAX_FILE_CHARS = 120_000
KIND_ORDER = ["bug", "test", "refactor", "improvement", "feature"]
KIND_HEADINGS = {
    "test": "Tests to add", "refactor": "Refactors", "improvement": "Improvements",
    "feature": "Ideas",
}
CAPS = {"test": 3, "refactor": 3, "improvement": 3, "feature": 2}
SEVERITIES = ["high", "medium", "low"]

EXAMPLE_CONFIG = {
    "context": "What this code is, in a sentence or two, for the reviewers.",
    "branch": "main",
    "first_run_commits": 20,
    "whole_files": True,
    "reviews_per_run": 2,
    "reviewers": [
        {
            "name": "gemini",
            "type": "api",
            "base_url": "https://generativelanguage.googleapis.com/v1beta/openai",
            "model": "gemini-pro-latest",
            "key_env": "GEMINI_API_KEY",
            "budget_tokens": 200000,
        },
        {
            "name": "openai",
            "type": "api",
            "base_url": "https://api.openai.com/v1",
            "model": "gpt-4.1",
            "key_env": "OPENAI_API_KEY",
            "budget_tokens": 100000,
        },
        {
            "name": "copilot",
            "type": "command",
            "command": [
                "copilot", "-p",
                "Read context.md in the current directory, all of it, and do exactly what its "
                "opening instructions say. Reply with the findings only.",
                "-s", "--available-tools", "view", "--allow-tool", "view",
            ],
            "budget_tokens": 60000,
        },
    ],
}


class ReviewerOut(Exception):
    """A reviewer could not answer: out of quota, no key, timed out."""


# ── git ──────────────────────────────────────────────────────────────────────

def git(*args: str, repo: Path, input: str | None = None, env: dict | None = None,
        check: bool = True) -> str:
    out = subprocess.run(
        ["git", "-C", str(repo), "-c", "core.quotePath=false", *args],
        input=input, capture_output=True, text=True, encoding="utf-8", errors="replace",
        env={**os.environ, **(env or {})},
    )
    if check and out.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {out.stderr.strip()}")
    return out.stdout


def is_ancestor(repo: Path, a: str, b: str) -> bool:
    return subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", a, b],
                          capture_output=True).returncode == 0


def has_commit(repo: Path, c: str) -> bool:
    return subprocess.run(["git", "-C", str(repo), "cat-file", "-e", f"{c}^{{commit}}"],
                          capture_output=True).returncode == 0


def reviewable(path: str) -> bool:
    parts = path.split("/")
    name = parts[-1]
    ext = name.rsplit(".", 1)[-1] if "." in name else ""
    is_test = (".test." in name or ".spec." in name or "__tests__" in parts or "e2e" in parts
               or name.startswith("test_"))
    return (ext in REVIEWABLE_EXTS and not SKIP_DIRS.intersection(parts) and not is_test
            and not name.endswith((".min.js", ".d.ts")))


def read_at(repo: Path, rev: str, path: str) -> str | None:
    out = subprocess.run(["git", "-C", str(repo), "show", f"{rev}:{path}"], capture_output=True)
    if out.returncode != 0 or b"\0" in out.stdout or len(out.stdout) > 200_000:
        return None
    try:
        return out.stdout.decode("utf-8")
    except UnicodeDecodeError:
        return None


def patch_applies(repo: Path, rev: str, patch: str) -> bool:
    """Whether `patch` applies to the tree at `rev`, in a scratch index."""
    with tempfile.TemporaryDirectory() as d:
        env = {"GIT_INDEX_FILE": str(Path(d) / "index")}
        try:
            git("read-tree", rev, repo=repo, env=env)
        except RuntimeError:
            return False
        out = subprocess.run(
            ["git", "-C", str(repo), "apply", "--cached", "--check", "--recount", "-"],
            input=patch, capture_output=True, text=True, env={**os.environ, **env},
        )
        return out.returncode == 0


# ── the pieces ported from the codereview crate ──────────────────────────────

def squash(s: str) -> str:
    return " ".join(s.split())


def numbered(content: str) -> str:
    cut = content[:MAX_FILE_CHARS]
    out = "\n".join(f"{i + 1:>5}| {line}" for i, line in enumerate(cut.splitlines()))
    if len(cut) < len(content):
        out += "\n  ...| (file cut here: too long to show in full)"
    return out


def estimate_tokens(text: str) -> int:
    return -(-len(text) // 7) * 2


def extract_json(text: str, open_ch: str):
    close_ch = "]" if open_ch == "[" else "}"
    start = 0
    while True:
        s = text.find(open_ch, start)
        if s < 0:
            return None
        depth, in_str, esc = 0, False, False
        for i in range(s, len(text)):
            c = text[i]
            if in_str:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == '"':
                    in_str = False
                continue
            if c == '"':
                in_str = True
            elif c == open_ch:
                depth += 1
            elif c == close_ch:
                depth -= 1
                if depth == 0:
                    try:
                        return json.loads(text[s:i + 1])
                    except ValueError:
                        break
        start = s + 1


def field(v: dict, *names: str) -> str:
    for n in names:
        x = v.get(n)
        if isinstance(x, (str, int, float)) and not isinstance(x, bool):
            return str(x).strip()
    return ""


def parse_kind(s: str) -> str:
    s = s.strip().lower()
    if s in ("test", "tests", "test case", "unit test", "missing test"):
        return "test"
    if s in ("refactor", "refactoring", "cleanup", "simplification"):
        return "refactor"
    if s in ("improvement", "enhancement", "improve"):
        return "improvement"
    if s in ("feature", "idea", "new feature"):
        return "feature"
    return "bug"


def parse_severity(s: str) -> str:
    s = s.strip().lower()
    if s in ("high", "critical", "blocker", "major"):
        return "high"
    if s in ("low", "minor", "nit", "trivial", "info"):
        return "low"
    return "medium"


def parse_findings(text: str) -> list[dict]:
    value = extract_json(text, "[")
    if value is None:
        obj = extract_json(text, "{")
        value = obj.get("findings", [obj]) if isinstance(obj, dict) else None
    if not isinstance(value, list):
        return []
    out = []
    for v in value:
        if not isinstance(v, dict):
            continue
        file = field(v, "file", "path", "filename")
        line = 0
        for n in ("line", "line_number", "lineNumber"):
            m = re.match(r"\d+", str(v.get(n, "")))
            if m:
                line = int(m.group())
                break
        m = re.match(r"^(.*):(\d+)$", file)
        if m:
            file, line = m.group(1), line or int(m.group(2))
        title, quote = field(v, "title", "summary", "issue"), field(v, "quote", "code", "snippet")
        if not (file and title and quote):
            continue
        kind = parse_kind(field(v, "kind", "type", "category"))
        out.append({
            "kind": kind, "file": file, "line": line, "title": title, "quote": quote,
            "severity": parse_severity(field(v, "severity", "priority")) if kind == "bug" else "low",
            "scenario": field(v, "explanation", "scenario", "failure_scenario", "impact", "why"),
            "fix": field(v, "fix", "suggested_fix", "suggestion"),
            "patch": field(v, "patch", "diff"),
        })
    return out


def parse_verdict(text: str) -> tuple[str, str]:
    v = extract_json(text, "{")
    if isinstance(v, dict):
        word = field(v, "verdict", "answer", "result").lower()
        verdict = ("confirmed" if word in ("confirmed", "yes", "real", "true")
                   else "rejected" if word in ("rejected", "no", "false", "not real") else "unsure")
        return verdict, field(v, "reason", "why", "explanation")
    low = text.strip().lower()
    verdict = ("confirmed" if low.startswith(("yes", "confirmed"))
               else "rejected" if low.startswith(("no", "rejected")) else "unsure")
    return verdict, text.strip()[:300]


def strip_diff_markers(quote: str) -> str:
    lines = [ln for ln in quote.splitlines() if ln]
    if (lines and all(ln.startswith(("+", " ")) for ln in lines)
            and any(ln.startswith("+") and not ln.startswith("++") for ln in lines)):
        return "\n".join(ln[1:] for ln in quote.splitlines())
    return quote


def locate_quote(content: str, quote: str, reported: int) -> int | None:
    q = squash(quote)
    if len(q) < 8:
        return None
    lines = content.splitlines()
    first_line = next((ln.strip() for ln in quote.splitlines() if ln.strip()), "")
    first = squash(first_line)
    best = None
    for i, line in enumerate(lines):
        if first not in squash(line):
            continue
        window = ""
        for ln in lines[i:]:
            # A blank line would add a second space, and a quote spanning one never matched
            if not squash(ln):
                continue
            window = (window + " " if window else "") + squash(ln)
            if len(window) >= len(q) + len(first) + 200:
                break
        if q in window:
            n = i + 1
            if best is None or abs(n - reported) < abs(best - reported):
                best = n
    return best


def normalize_patch(patch: str, repo_name: str) -> str:
    lines = patch.strip().splitlines()
    if lines and lines[0].lstrip().startswith("```"):
        lines = lines[1:]
    if lines and lines[-1].lstrip().startswith("```"):
        lines = lines[:-1]
    if not any(ln.startswith("@@") for ln in lines):
        return ""
    out = []
    for ln in lines:
        for p in ("--- a/", "+++ b/", "--- ", "+++ "):
            if ln.startswith(p):
                rest = ln[len(p):]
                if rest.startswith(repo_name + "/"):
                    rest = rest[len(repo_name) + 1:]
                ln = p + rest
                break
        out.append(ln)
    return "\n".join(out) + "\n"


def finding_key(kind: str, repo_name: str, path: str, quote: str) -> str:
    """The home system's id (codereview::dedupe::finding_key), FNV-1a."""
    prefix = "" if kind == "bug" else f"{kind}\0"
    h = 0xCBF29CE484222325
    for b in f"{prefix}{repo_name}\0{path}\0{squash(quote)}".encode():
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


def excerpt(content: str, line: int, radius: int) -> str:
    start, end = max(1, line - radius), line + radius
    return "\n".join(f"{n:>5}| {t}" for n, t in enumerate(content.splitlines(), 1)
                     if start <= n <= end)


# ── the context: who calls the changed code ─────────────────────────────────

def changed_function_names(diff: str) -> list[str]:
    """Names of the functions a diff touches. With -W each hunk is a whole
    function, so its definition line is in the hunk (or, when the hunk starts
    below it, in git's hunk header)."""
    names: list[str] = []
    call = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)\s*\(")
    control = {"if", "for", "while", "switch", "return", "sizeof", "catch", "else", "do",
               "function", "def", "fn", "new", "await", "throw"}
    for line in diff.splitlines():
        if line.startswith("@@"):
            text = line.split("@@", 2)[-1].strip()
        elif line[:1] in (" ", "+") and not line.startswith("+++"):
            text = line[1:].strip()
            # A definition opens a body; a call or a declaration ends in ";".
            if not text.endswith(("{", ":", ")")) or text.startswith(("//", "#", "*", "/*")):
                continue
        else:
            continue
        for m in call.finditer(text):
            n = m.group(1)
            if n in control:
                continue
            if len(n) >= 3 and n not in names:
                names.append(n)
            break
    return names[:8]


def callers(repo: Path, rev: str, names: list[str], exclude: set[str]) -> list[str]:
    out: list[str] = []
    for name in names:
        res = subprocess.run(
            ["git", "-C", str(repo), "grep", "-n", "-w", "-I", "-F", "-e", name, rev, "--"],
            capture_output=True, text=True, encoding="utf-8", errors="replace")
        hits = 0
        for ln in res.stdout.splitlines():
            ln = ln[len(rev) + 1:] if ln.startswith(rev + ":") else ln
            path, _, rest = ln.partition(":")
            num, _, text = rest.partition(":")
            if path in exclude or not num.isdigit():
                continue
            out.append(f"- {path}:{num}  {text.strip()[:160]}")
            hits += 1
            if hits == 4:
                break
    return out


# ── reviewers ────────────────────────────────────────────────────────────────

def call_api(r: dict, system: str, user: str, max_tokens: int) -> str:
    # No key_env: a local server that needs none (mac1's coder)
    key = os.environ.get(r["key_env"], "") if r.get("key_env") else "none"
    if not key:
        raise ReviewerOut(f"{r['name']}: ${r['key_env']} is not set")
    body = json.dumps({
        "model": r["model"], "temperature": 0.2, "max_tokens": max_tokens,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
    }).encode()
    req = urllib.request.Request(
        r["base_url"].rstrip("/") + "/chat/completions", data=body, method="POST",
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}"})
    try:
        with urllib.request.urlopen(req, timeout=r.get("timeout_secs", 600)) as resp:
            data = json.load(resp)
    except urllib.error.HTTPError as e:
        detail = e.read().decode("utf-8", "replace")[:300]
        raise ReviewerOut(f"{r['name']}: HTTP {e.code}: {detail}") from e
    except (urllib.error.URLError, TimeoutError) as e:
        raise ReviewerOut(f"{r['name']}: {e}") from e
    try:
        return data["choices"][0]["message"]["content"] or ""
    except (KeyError, IndexError, TypeError) as e:
        raise ReviewerOut(f"{r['name']}: no answer in the reply") from e


def windows_ssh_script(cmd: list[str], folder: str) -> str:
    """PowerShell for a Windows host: run the command in the folder scp just
    copied context.md into (under the home folder), then remove the folder.
    Not stdin: a prompt of 13k tokens on stdin hung [Console]::In for good."""
    quoted = " ".join("'" + a.replace("'", "''") + "'" for a in cmd[1:])
    return (
        "$ErrorActionPreference = 'Stop'\n"
        "[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false\n"
        f"$d = Join-Path $HOME '{folder}'\n"
        "try {\n"
        "  Set-Location $d\n"
        # The program, not npm's .ps1 shim, which script policy blocks
        f"  $exe = (Get-Command '{cmd[0]}' -CommandType Application | Select-Object -First 1).Source\n"
        f"  & $exe {quoted}\n"
        "  $rc = $LASTEXITCODE\n"
        "} finally { Set-Location $HOME; Remove-Item -Recurse -Force $d }\n"
        "exit $rc\n"
    )


def call_command(r: dict, system: str, user: str) -> str:
    """A CLI reviewer (Copilot): the prompt goes in context.md, the command
    runs beside it, and what it prints is the reply. With "ssh_host" the
    command runs on that Windows machine instead (beelink1's Copilot)."""
    cmd = list(r["command"])
    prompt = system + "\n\n" + user
    timeout = r.get("timeout_secs", 1800)
    if r.get("ssh_host"):
        host = r["ssh_host"]
        ssh_opts = ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]
        with tempfile.TemporaryDirectory() as d:
            folder = "review-diff-" + os.urandom(6).hex()
            Path(d, folder).mkdir()
            Path(d, folder, "context.md").write_text(prompt, encoding="utf-8")
            sent = subprocess.run(["scp", "-q", "-r", *ssh_opts, str(Path(d, folder)), f"{host}:"],
                                  capture_output=True, text=True, timeout=120)
            if sent.returncode != 0:
                raise ReviewerOut(f"{r['name']}: can't copy the prompt to {host}: "
                                  f"{sent.stderr.strip()[-300:]}")
        script = windows_ssh_script(cmd, folder).encode("utf-16-le")
        remote = ["ssh", *ssh_opts, host, "powershell", "-NoProfile", "-NonInteractive",
                  "-EncodedCommand", base64.b64encode(script).decode()]
        try:
            out = subprocess.run(remote, stdin=subprocess.DEVNULL, capture_output=True,
                                 text=True, encoding="utf-8", errors="replace", timeout=timeout)
        except subprocess.TimeoutExpired as e:
            raise ReviewerOut(f"{r['name']}: timed out") from e
        if out.returncode == 255:
            raise ReviewerOut(f"{r['name']}: can't reach {host}: {out.stderr.strip()[-300:]}")
    else:
        exe = shutil.which(cmd[0])
        if not exe:
            raise ReviewerOut(f"{r['name']}: {cmd[0]} is not installed")
        cmd[0] = exe
        if os.name == "nt" and exe.lower().endswith((".cmd", ".bat")):
            cmd = ["cmd.exe", "/c", *cmd]
        with tempfile.TemporaryDirectory() as d:
            Path(d, "context.md").write_text(prompt, encoding="utf-8")
            try:
                out = subprocess.run(cmd, cwd=d, capture_output=True, text=True, encoding="utf-8",
                                     errors="replace", timeout=timeout)
            except subprocess.TimeoutExpired as e:
                raise ReviewerOut(f"{r['name']}: timed out") from e
    text = out.stdout + out.stderr
    if re.search(r"(?i)quota|rate limit|insufficient.*credit|out of credits", text):
        raise ReviewerOut(f"{r['name']}: out of quota")
    if out.returncode != 0 or not out.stdout.strip():
        raise ReviewerOut(f"{r['name']}: exit {out.returncode}: {out.stderr.strip()[:300]}")
    return out.stdout


def ask(r: dict, system: str, user: str, max_tokens: int = 8192) -> str:
    if r.get("type", "api") == "command":
        return call_command(r, system, user)
    return call_api(r, system, user, max_tokens)


def label(r: dict) -> str:
    return f"{r.get('model') or r['name']}@{r['name']}"


# ── a run ────────────────────────────────────────────────────────────────────

def repo_name(repo: Path) -> str:
    """The folder's name; a bare mirror (lenovo1's copy of a work repo)
    without its .git."""
    if git("rev-parse", "--is-bare-repository", repo=repo).strip() == "true":
        return Path(git("rev-parse", "--absolute-git-dir", repo=repo).strip()).name.removesuffix(".git")
    return Path(git("rev-parse", "--show-toplevel", repo=repo).strip()).name


class Run:
    def __init__(self, repo: Path, cfg: dict, prompts: Path, out=print):
        self.repo = repo
        self.cfg = cfg
        self.out = out
        self.name = repo_name(repo)
        self.state_dir = Path(git("rev-parse", "--absolute-git-dir", repo=repo).strip()) / "review-diff"
        self.state_dir.mkdir(parents=True, exist_ok=True)
        self.review_md = (prompts / "review.md").read_text(encoding="utf-8")
        self.verify_md = (prompts / "verify.md").read_text(encoding="utf-8")

    # state: the bookmark, the turn, and the ledger for the scorecard
    def load(self, name: str, default):
        p = self.state_dir / name
        try:
            return json.loads(p.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return default

    def save(self, name: str, value) -> None:
        (self.state_dir / name).write_text(json.dumps(value, indent=1), encoding="utf-8")

    def apply_marks(self) -> None:
        """Outcomes left as files, review-diff/marks/<id> holding fixed or
        dismissed: how Claude Code records them on the work laptop, whose
        marks the lenovo1 bridge copies into its mirror."""
        d = self.state_dir / "marks"
        if not d.is_dir():
            return
        ledger = self.load("ledger.json", {"raised": [], "findings": {}})
        for f in sorted(d.iterdir()):
            status = f.read_text(encoding="utf-8", errors="replace").strip()
            if f.name in ledger["findings"] and status in ("fixed", "dismissed", "open"):
                ledger["findings"][f.name]["status"] = status
                self.out(f"{f.name} marked {status}")
            else:
                self.out(f"mark {f.name} ({status!r}) matches no finding, dropped")
            f.unlink()
        self.save("ledger.json", ledger)

    def first_run_base(self, upto: str) -> str:
        """Where a first run starts: back along the branch while the range holds
        at most first_run_commits commits. Counting the branch's own commits
        alone would let one merge of main bring in hundreds; the commit at upto
        is always reviewed, however big a merge it is."""
        n = int(self.cfg.get("first_run_commits", 20))
        line = git("rev-list", "--first-parent", f"--max-count={n + 1}", upto,
                   repo=self.repo).split()
        if len(line) < 2:
            return line[-1]
        base = line[1]
        for c in line[2:]:
            if int(git("rev-list", "--count", f"{c}..{upto}", repo=self.repo)) > n:
                break
            base = c
        return base

    def review(self, upto: str, dry_run: bool = False, tested: bool = False,
               since: str | None = None) -> Path | None:
        repo = self.repo
        upto = git("rev-parse", "--verify", f"{upto}^{{commit}}", repo=repo).strip()
        self.apply_marks()
        state = self.load("state.json", {})
        last = state.get("last")
        if since:
            # A PR: everything since it left its base, bookmark or not
            last = git("rev-parse", "--verify", f"{since}^{{commit}}", repo=repo).strip()
        if last and has_commit(repo, last) and is_ancestor(repo, upto, last):
            self.out(f"nothing new since the last review ({last[:7]})")
            return None
        if last and has_commit(repo, last) and is_ancestor(repo, last, upto):
            base = last
        else:
            base = self.first_run_base(upto)
        paths = [p for p in git("diff", "--name-only", "--diff-filter=AMR", "-M", base, upto,
                                repo=repo).splitlines() if p and reviewable(p)]
        count = git("rev-list", "--count", f"{base}..{upto}", repo=repo).strip()
        scope = f"{count} new commits ({base[:7]}..{upto[:7]})"
        if tested:
            scope = "Tests passed: " + scope
        files = {}
        for p in paths:
            content = read_at(repo, upto, p)
            if content is not None:
                files[p] = content
        if not files:
            self.out("no reviewable files changed")
            if not dry_run:
                state["last"] = upto
                self.save("state.json", state)
            return None

        enabled = [r for r in self.cfg.get("reviewers", []) if not r.get("disabled")]
        reviewers = [r for r in enabled if not r.get("check_only")]
        checkers = [r for r in enabled if r.get("check_only")]
        if not reviewers:
            raise SystemExit("no reviewers in the config")
        turn = int(state.get("turn", 0))
        order = reviewers[turn % len(reviewers):] + reviewers[:turn % len(reviewers)]
        wanted = max(1, int(self.cfg.get("reviews_per_run", 2)))

        groups = self.prompts_for(base, upto, files, scope)
        if dry_run:
            self.out(f"{scope}: {len(files)} files, {len(groups)} prompt(s), "
                     f"first look: {order[0]['name']}")
            for g in groups:
                self.out(f"  {estimate_tokens(g)} tokens")
            return None

        raised: list[tuple[str, str, str | None]] = []
        findings: dict[str, dict] = {}
        dropped: list[dict] = []
        done_by: dict[str, int] = {}
        reviewed = 0
        for r in order:
            if reviewed == wanted:
                break
            budget = int(r.get("budget_tokens", 60000))
            if any(estimate_tokens(self.review_md) + estimate_tokens(g) > budget for g in groups):
                self.out(f"{r['name']}: the change is too big for its budget, skipped")
                continue
            try:
                replies = [ask(r, self.review_md, g) for g in groups]
            except ReviewerOut as e:
                self.out(f"{e}; trying the next reviewer")
                continue
            reviewed += 1
            who = label(r)
            done_by[who] = done_by.get(who, 0) + len(groups)
            for reply in replies:
                for raw in parse_findings(reply):
                    path = self.match_file(raw["file"], files)
                    quote = strip_diff_markers(raw["quote"])
                    line = locate_quote(files[path], quote, raw["line"]) if path else None
                    if line is None:
                        dropped.append({
                            "who": who, "kind": raw["kind"], "file": raw["file"],
                            "path": path, "line": raw["line"], "title": raw["title"],
                            "quote": quote, "severity": raw["severity"],
                            "scenario": raw["scenario"], "fix": raw["fix"],
                            "patch": normalize_patch(raw["patch"], self.name),
                            "found_model": r.get("model") or r["name"],
                            "why": "the quoted code is not in it" if path else
                                   "no such file under review",
                        })
                        raised.append((who, raw["kind"], None))
                        continue
                    fid = finding_key(raw["kind"], self.name, path, quote)
                    raised.append((who, raw["kind"], fid))
                    if fid in findings:
                        if who not in findings[fid]["found_by"]:
                            findings[fid]["found_by"].append(who)
                        continue
                    findings[fid] = {
                        "id": fid, "kind": raw["kind"], "path": path, "line": line,
                        "severity": raw["severity"], "title": raw["title"], "quote": quote,
                        "scenario": raw["scenario"], "fix": raw["fix"],
                        "patch": normalize_patch(raw["patch"], self.name), "found_by": [who],
                        "found_model": r.get("model") or r["name"],
                    }
        if reviewed == 0:
            raise SystemExit("no reviewer could answer; nothing reviewed, the bookmark stays")

        for f in findings.values():
            self.check(f, files, order + checkers, done_by)
        # A misquote isn't proof of a made-up bug (one real one was lost that
        # way): another reviewer judges each against the code all the same
        for d in dropped:
            if d["path"]:
                self.check(d, files, order + checkers, done_by, misquoted=True)
            if f["patch"]:
                f["patch_applies"] = patch_applies(repo, upto, f["patch"])
        kept, left_out = cap(dedupe(list(findings.values())))
        report = render(self.name, scope, done_by, len(files), dropped, left_out, kept)

        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M")
        reports = self.state_dir / "reports"
        inbox = self.state_dir / "inbox"
        reports.mkdir(exist_ok=True)
        inbox.mkdir(exist_ok=True)
        path = reports / f"{stamp}-{upto[:7]}.md"
        path.write_text(report, encoding="utf-8")
        (reports / "latest.md").write_text(report, encoding="utf-8")
        if any(f.get("verdict") != "rejected" for f in kept):
            (inbox / path.name).write_text(report, encoding="utf-8")

        ledger = self.load("ledger.json", {"raised": [], "findings": {}})
        seen = {(x["reviewer"], x["id"]) for x in ledger["raised"] if x.get("id")}
        for who, kind, fid in raised:
            if fid is None or (who, fid) not in seen:
                ledger["raised"].append({"reviewer": who, "kind": kind, "id": fid})
                seen.add((who, fid))
        for f in kept + [f for f in findings.values() if f not in kept]:
            entry = ledger["findings"].setdefault(f["id"], {"status": "open"})
            entry.update({"kind": f["kind"], "verdict": f.get("verdict"), "title": f["title"],
                          "path": f["path"]})
        self.save("ledger.json", ledger)
        state.update({"last": upto, "turn": turn + 1})
        self.save("state.json", state)
        self.out(headline(self.name, kept) or f"{self.name}: nothing found")
        self.out(f"report: {path}")
        return path

    def prompts_for(self, base: str, upto: str, files: dict, scope: str) -> list[str]:
        """The review prompts: each changed file with its diff and whole
        text, and who calls the changed functions, grouped to stay under the
        smallest reviewer's budget."""
        budget = int(self.cfg.get("prompt_tokens") or min(
            int(r.get("budget_tokens", 60000)) for r in self.cfg.get("reviewers", [])
            if not r.get("disabled") and not r.get("check_only")))
        budget -= estimate_tokens(self.review_md) + 2000
        head = f"Repository: {self.name}\nThis review covers: {scope}\n"
        if self.cfg.get("context"):
            head += f"About this code: {self.cfg['context']}\n"
        blocks = []
        for path, content in files.items():
            diff = git("diff", "-W", base, upto, "--", path, repo=self.repo)
            calls = callers(self.repo, upto, changed_function_names(diff), {path})
            block = f"\n=== FILE: {self.name}/{path} (UNDER REVIEW) ===\n"
            block += "--- what changed (unified diff, each changed function whole) ---\n"
            block += diff.rstrip() + "\n"
            if calls:
                block += "--- where the changed functions are called ---\n" + "\n".join(calls) + "\n"
            if self.cfg.get("whole_files", True):
                block += "--- full file as it is now ---\n" + numbered(content) + "\n"
            blocks.append(block)
        groups, cur = [], head
        for b in blocks:
            if cur != head and estimate_tokens(cur + b) > budget:
                groups.append(cur)
                cur = head
            cur += b
        groups.append(cur)
        tail = "\nReport what you find in the files UNDER REVIEW as a JSON array."
        return [g + tail for g in groups]

    def match_file(self, name: str, files: dict) -> str | None:
        name = name.strip().lstrip("./")
        if name.startswith(self.name + "/"):
            name = name[len(self.name) + 1:]
        if name in files:
            return name
        return next((p for p in files if p.endswith(name) or name.endswith(p)), None)

    def check(self, f: dict, files: dict, order: list[dict], done_by: dict,
              misquoted: bool = False) -> None:
        """Ask a different reviewer to check a finding against the file: the
        config's "checker" first (mac1's coder), then an API one (quicker);
        the finder only if no other will answer."""
        others = [r for r in order if (r.get("model") or r["name"]) != f["found_model"]]
        others.sort(key=lambda r: (r["name"] != self.cfg.get("checker"),
                                   r.get("type", "api") == "command"))
        same = [r for r in order if r not in others]
        content = files[f["path"]]
        user = (f"The item:\n- kind: {f['kind']}\n- location: {self.name}/{f['path']}:{f['line']}\n"
                f"- severity: {f['severity'].capitalize()}\n- title: {f['title']}\n"
                f"- quoted code: {f['quote']}\n- explanation: {f['scenario']}\n"
                f"- suggested change: {f['fix']}\n")
        if f["patch"]:
            user += "- the reviewer's patch:\n" + f["patch"].rstrip() + "\n"
        if misquoted:
            user += ("- note: the quoted code is not in the file word for word. Judge the "
                     "claim against the code as it is; a misquote alone is not a reason to reject.\n")
        if not self.cfg.get("whole_files", True):
            shown, what = excerpt(content, f["line"], 60), "the lines around the item"
        elif estimate_tokens(content) < 40_000:
            shown, what = numbered(content), "the file the item is in"
        else:
            shown, what = excerpt(content, f["line"], 120), "the file the item is in"
        user += f"\n=== FILE: {self.name}/{f['path']} ({what}) ===\n{shown}\n"
        user += "\nDoes this item hold up against the code? Answer with the JSON object only."
        for r in others + same:
            try:
                reply = ask(r, self.verify_md, user, 512)
            except ReviewerOut as e:
                self.out(f"{e}; asking another checker")
                continue
            f["verdict"], f["verdict_reason"] = parse_verdict(reply)
            f["checked_by"] = label(r)
            done_by[label(r)] = done_by.get(label(r), 0) + 1
            return


def dedupe(findings: list[dict]) -> list[dict]:
    out: list[dict] = []
    for f in findings:
        same = next((e for e in out if e["kind"] == f["kind"] and e["path"] == f["path"] and (
            e["line"] == f["line"] or squash(e["quote"]) == squash(f["quote"]))), None)
        if same is None:
            out.append(f)
            continue
        if SEVERITIES.index(f["severity"]) < SEVERITIES.index(same["severity"]):
            same["severity"] = f["severity"]
        same["found_by"] += [w for w in f["found_by"] if w not in same["found_by"]]
        if len(f["scenario"]) > len(same["scenario"]):
            same["scenario"] = f["scenario"]
        if not same["patch"]:
            same["patch"] = f["patch"]
    out.sort(key=lambda f: (KIND_ORDER.index(f["kind"]), SEVERITIES.index(f["severity"]),
                            f["path"], f["line"]))
    return out


def cap(findings: list[dict]) -> tuple[list[dict], int]:
    rank = {"confirmed": 0, "unsure": 1, None: 2, "rejected": 3}
    drop = set()
    for kind, n in CAPS.items():
        mine = [f for f in findings if f["kind"] == kind and f.get("verdict") != "rejected"]
        mine.sort(key=lambda f: rank.get(f.get("verdict"), 2))
        drop.update(f["id"] for f in mine[n:])
    return [f for f in findings if f["id"] not in drop], len(drop)


def block(n: int, f: dict, repo_name: str, label_kind: bool) -> str:
    kind = f"{f['kind']}: " if label_kind and f["kind"] != "bug" else ""
    out = f"### {n}. {kind}{f['title']}\n"
    out += f"- **Where:** `{repo_name}/{f['path']}:{f['line']}` (id `{f['id']}`)\n"
    if f["scenario"]:
        out += f"- **{'What goes wrong' if f['kind'] == 'bug' else 'Why'}:** {f['scenario']}\n"
    if f["fix"]:
        out += f"- **Change:** {f['fix']}\n"
    by, v = f.get("checked_by"), f.get("verdict")
    checked = (f"confirmed by {by}" if v == "confirmed" else f"{by} could not tell" if v == "unsure"
               else f"rejected by {by}" if v == "rejected" else "not checked (no other reviewer answered)")
    out += f"- **Found by:** {', '.join(f['found_by'])}; {checked}"
    if f.get("verdict_reason"):
        out += f" — {f['verdict_reason']}"
    lang = f["path"].rsplit(".", 1)[-1]
    out += f"\n\n```{lang}\n{f['quote'].rstrip()}\n```\n"
    if f["patch"].strip():
        note = {True: " (applies cleanly)",
                False: " (does not apply cleanly to the code as it is now: a starting point only)"
                }.get(f.get("patch_applies"), "")
        out += f"\n**Diff**{note}:\n\n```diff\n{f['patch'].rstrip()}\n```\n"
    return out + "\n"


def render(repo_name: str, scope: str, done_by: dict, n_files: int, dropped: list[dict],
           left_out: int, findings: list[dict]) -> str:
    """The report, laid out as the home system's (codereview::report::render)."""
    confirmed = [f for f in findings if f.get("verdict") == "confirmed"]
    unconfirmed = [f for f in findings if f.get("verdict") in (None, "unsure")]
    rejected = sum(f.get("verdict") == "rejected" for f in findings)
    bugs = {s: sum(f["kind"] == "bug" and f["severity"] == s for f in confirmed) for s in SEVERITIES}
    of = {k: sum(f["kind"] == k for f in confirmed) for k in KIND_ORDER}
    out = f"# Code review: {repo_name}\n\n- **Scope:** {scope}\n"
    out += f"- **Finished:** {dt.datetime.now():%Y-%m-%d %H:%M}\n"
    out += f"- **Size:** {n_files} files under review\n"
    out += f"- **Done by:** {', '.join(f'{w} ({n} tasks)' for w, n in done_by.items())}\n"
    out += (f"- **Confirmed bugs:** {bugs['high']} high, {bugs['medium']} medium, {bugs['low']} low. "
            f"**Suggestions:** {of['test']} tests, {of['refactor']} refactors, "
            f"{of['improvement']} improvements, {of['feature']} ideas.\n")
    extra = f", {left_out} suggestions over the per-kind limit" if left_out else ""
    out += (f"- **Not confirmed:** {len(unconfirmed)}. **Thrown out:** {rejected} rejected by the "
            f"checker, {len(dropped)} quoting code that is not in the file{extra}.\n\n")
    out += ("Written by models. Treat every item as a lead to check, not a verdict: fix each bug, "
            "test, refactor and improvement (the diff is a starting point), or drop it with the "
            "reason. Ideas are for Jon to choose from. Record what became of each item for the "
            "reviewers' scorecard: `review_diff.py mark <id> fixed` (or `dismissed`).\n\n")
    if not confirmed and not unconfirmed:
        return out + "Nothing found in this run.\n" + dropped_section(dropped)
    n = 0
    for sev in SEVERITIES:
        group = [f for f in confirmed if f["kind"] == "bug" and f["severity"] == sev]
        if group:
            out += f"## Bugs: {sev}\n\n"
            for f in group:
                n += 1
                out += block(n, f, repo_name, False)
    for kind in KIND_ORDER[1:]:
        group = [f for f in confirmed if f["kind"] == kind]
        if group:
            out += f"## {KIND_HEADINGS[kind]}\n\n"
            for f in group:
                n += 1
                out += block(n, f, repo_name, False)
    if unconfirmed:
        out += ("## Not confirmed\n\nThe checker could not tell, or no other reviewer answered. "
                "Worth a look, with more doubt.\n\n")
        for f in unconfirmed:
            n += 1
            out += block(n, f, repo_name, True)
    return out + dropped_section(dropped)


def dropped_section(dropped: list[dict]) -> str:
    """What the quote check threw out, so a reviewer misquoting a real bug can
    be told from one making things up. Not items to act on."""
    if not dropped:
        return ""
    out = ("\n## Misquoted: quoted code not in the file\n\nAnother reviewer judged each "
           "against the code anyway. A confirmed one is worth a look, with the most doubt of "
           "anything here.\n\n")
    for d in dropped:
        out += (f"- **{d['title'] or '(no title)'}** ({d['kind']}, {d['who']}): "
                f"`{d['file']}:{d['line']}`, {d['why']}.")
        if d.get("verdict"):
            out += f" **{d['verdict'].capitalize()}** by {d['checked_by']}: {d['verdict_reason']}"
        elif d.get("path"):
            out += " No other reviewer answered."
        if d.get("scenario"):
            out += f"\n  {d['scenario']}"
        out += " Quoted:\n\n"
        out += "  ```\n" + "\n".join("  " + l for l in (d["quote"] or "(nothing)").splitlines()) + "\n  ```\n"
    return out


def headline(repo_name: str, findings: list[dict]) -> str | None:
    c = [f for f in findings if f.get("verdict") == "confirmed"]
    h, m, lo = (sum(f["kind"] == "bug" and f["severity"] == s for f in c) for s in SEVERITIES)
    s = sum(f["kind"] != "bug" for f in c)
    if h + m + lo + s == 0:
        return None
    line = f"{repo_name}: {h} high, {m} medium, {lo} low"
    return line + (f"; {s} suggestion{'s' if s != 1 else ''}" if s else "")


# ── the scorecard ────────────────────────────────────────────────────────────

def scorecard(ledger: dict) -> list[dict]:
    rows: dict[tuple[str, str], dict] = {}
    for x in ledger.get("raised", []):
        key = (x["reviewer"], x["kind"])
        r = rows.setdefault(key, {"reviewer": x["reviewer"], "kind": x["kind"], "raised": 0,
                                  "invented": 0, "rejected": 0, "unsure": 0, "confirmed": 0,
                                  "fixed": 0, "dismissed": 0})
        r["raised"] += 1
        if not x.get("id"):
            r["invented"] += 1
            continue
        f = ledger.get("findings", {}).get(x["id"], {})
        if f.get("verdict") in ("rejected", "unsure", "confirmed"):
            r[f["verdict"]] += 1
        if f.get("status") in ("fixed", "dismissed"):
            r[f["status"]] += 1
    return sorted(rows.values(), key=lambda r: (r["reviewer"], KIND_ORDER.index(r["kind"])))


def score_text(ledger: dict) -> str:
    totals: dict[str, dict] = {}
    for r in scorecard(ledger):
        t = totals.setdefault(r["reviewer"], {k: 0 for k in r if k not in ("reviewer", "kind")})
        for k in t:
            t[k] += r[k]
    if not totals:
        return "No findings raised yet."
    pct = lambda n, d: f"{round(100 * n / d)}%" if d else "—"  # noqa: E731
    head = f"{'Reviewer':<32}{'Raised':>8}{'Made up':>9}{'Rejected':>10}{'Confirmed':>11}{'Fixed':>7}{'Dismissed':>11}{'Conf %':>8}{'Fixed %':>9}"
    lines = [head]
    for who, t in sorted(totals.items(), key=lambda kv: -kv[1]["raised"]):
        lines.append(f"{who:<32}{t['raised']:>8}{t['invented']:>9}{t['rejected']:>10}"
                     f"{t['confirmed']:>11}{t['fixed']:>7}{t['dismissed']:>11}"
                     f"{pct(t['confirmed'], t['raised']):>8}"
                     f"{pct(t['fixed'], t['fixed'] + t['dismissed']):>9}")
    return "\n".join(lines)


# ── command line ─────────────────────────────────────────────────────────────

def load_config(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise SystemExit(f"no config at {path}: run `review_diff.py init` first")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="review_diff.py", description=__doc__.split("\n\n")[0])
    ap.add_argument("--repo", default=".", help="the git repo (default: here)")
    ap.add_argument("--config", default=str(DEFAULT_CONFIG))
    ap.add_argument("--prompts", default=str(DEFAULT_PROMPTS))
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("init", help="write an example config")
    run_p = sub.add_parser("run", help="review the commits since the last run")
    run_p.add_argument("--upto", default="HEAD", help="the commit that passed the tests")
    run_p.add_argument("--dry-run", action="store_true", help="say what would be sent, send nothing")
    run_p.add_argument("--since", help="review from this commit (a PR's base), not the bookmark")
    run_p.add_argument("--only", help="ask just this reviewer from the config (e.g. copilot)")
    run_p.add_argument("--tested", action="store_true",
                       help="the tests passed at --upto (after-tests.sh says so)")
    mark_p = sub.add_parser("mark", help="record what became of a finding")
    mark_p.add_argument("id")
    mark_p.add_argument("status", choices=["fixed", "dismissed", "open"])
    sub.add_parser("score", help="each reviewer's record")
    a = ap.parse_args(argv)

    if a.cmd == "init":
        path = Path(a.config)
        if path.exists():
            print(f"{path} already exists")
            return 1
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(EXAMPLE_CONFIG, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {path}: set the keys' environment variables and the context line")
        return 0
    repo = Path(a.repo).resolve()
    cfg = load_config(Path(a.config)) if a.cmd == "run" else {}
    if a.cmd == "run" and a.only:
        cfg["reviewers"] = [r for r in cfg.get("reviewers", []) if r["name"] == a.only]
        if not cfg["reviewers"]:
            raise SystemExit(f"no reviewer named {a.only} in {a.config}")
        cfg["reviewers"][0].pop("disabled", None)
    # One config for several repos: "contexts" says what each one is
    if a.cmd == "run":
        name = repo_name(Path(a.repo).resolve())
        cfg["context"] = cfg.get("contexts", {}).get(name, cfg.get("context", ""))
    run = Run(repo, cfg, Path(a.prompts))
    if a.cmd == "run":
        run.review(a.upto, a.dry_run, a.tested, a.since)
    elif a.cmd == "mark":
        ledger = run.load("ledger.json", {"raised": [], "findings": {}})
        if a.id not in ledger["findings"]:
            print(f"no finding {a.id} in this repo's ledger")
            return 1
        ledger["findings"][a.id]["status"] = a.status
        run.save("ledger.json", ledger)
        print(f"{a.id} marked {a.status}")
    elif a.cmd == "score":
        run.apply_marks()
        print(score_text(run.load("ledger.json", {})))
    return 0


if __name__ == "__main__":
    sys.exit(main())
