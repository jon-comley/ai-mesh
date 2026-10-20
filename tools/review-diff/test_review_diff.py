"""Tests for review_diff.py: python3 -m unittest discover tools/review-diff"""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import review_diff as rd


def sh(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t",
                    "-c", "commit.gpgsign=false", *args], check=True, capture_output=True)


def make_repo(root: Path) -> Path:
    repo = root / "firmware"
    repo.mkdir()
    sh(repo, "init", "-q", "-b", "main")
    (repo / "src").mkdir()
    (repo / "src/net.c").write_text(
        "int net_send(int len) {\n  return len;\n}\n", encoding="utf-8")
    (repo / "src/main.c").write_text(
        "int main(void) {\n  return net_send(320);\n}\n", encoding="utf-8")
    sh(repo, "add", ".")
    sh(repo, "commit", "-qm", "one")
    return repo


REVIEW_REPLY = json.dumps([
    {"kind": "bug", "file": "firmware/src/net.c", "line": 2, "severity": "high",
     "title": "A negative length is sent on", "quote": "+  if (len > 512) return -1;",
     "explanation": "net_send(-1) returns -1 as if it had sent", "fix": "reject len < 0",
     "patch": "--- a/firmware/src/net.c\n+++ b/firmware/src/net.c\n@@ -1,4 +1,4 @@\n int net_send(int len) {\n-  if (len > 512) return -1;\n+  if (len < 0 || len > 512) return -1;\n   return len;\n }\n"},
    {"kind": "test", "file": "src/net.c", "line": 1, "title": "Test the 512 limit",
     "quote": "int net_send(int len) {", "explanation": "nothing tests it",
     "patch": "--- /dev/null\n+++ b/test/test_net.c\n@@ -0,0 +1 @@\n+void test_limit(void) {}\n"},
    {"kind": "bug", "file": "src/net.c", "line": 9, "title": "Invented",
     "quote": "memcpy(buf, src, 4096);"},
])


class Fake:
    """Stands in for the AI accounts: records each prompt, answers by role."""

    def __init__(self, out_of_quota=()):
        self.calls = []
        self.out_of_quota = set(out_of_quota)

    def __call__(self, r, system, user, max_tokens=8192):
        self.calls.append((r["name"], "check" if '"verdict"' in system else "review", user))
        if r["name"] in self.out_of_quota:
            raise rd.ReviewerOut(f"{r['name']}: HTTP 429")
        if '"verdict"' in system:
            return '{"verdict": "confirmed", "reason": "line 2 allows it"}'
        return REVIEW_REPLY


CFG = {
    "context": "Firmware for a data logger.",
    "reviewers": [
        {"name": "gemini", "type": "api", "model": "gemini-pro", "budget_tokens": 200000},
        {"name": "openai", "type": "api", "model": "gpt-x", "budget_tokens": 100000},
        {"name": "copilot", "type": "command", "command": ["copilot"], "budget_tokens": 60000},
    ],
}
PROMPTS = Path(__file__).resolve().parents[2] / "codereview" / "prompts"


class ReviewDiffTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = make_repo(Path(self.tmp.name))
        (self.repo / "src/net.c").write_text(
            "int net_send(int len) {\n  if (len > 512) return -1;\n  return len;\n}\n",
            encoding="utf-8")
        sh(self.repo, "commit", "-qam", "limit")
        self.log = []

    def tearDown(self):
        self.tmp.cleanup()

    def run_once(self, fake):
        with mock.patch.object(rd, "ask", fake):
            return rd.Run(self.repo, CFG, PROMPTS, out=self.log.append).review("HEAD")

    def test_a_run_reviews_checks_reports_and_keeps_score(self):
        fake = Fake()
        report = self.run_once(fake).read_text(encoding="utf-8")
        reviews = [c for c in fake.calls if c[1] == "review"]
        self.assertEqual([c[0] for c in reviews], ["gemini", "openai"], "first look and a second")
        prompt = reviews[0][2]
        self.assertIn("About this code: Firmware", prompt)
        self.assertIn("what changed (unified diff, each changed function whole)", prompt)
        self.assertIn("- src/main.c:2  return net_send(320);", prompt, "who calls it")
        checks = [c for c in fake.calls if c[1] == "check"]
        self.assertTrue(checks and all(c[0] == "openai" for c in checks),
                        "a different model checks gemini's finds (openai found them too)")
        self.assertIn("## Bugs: high", report)
        self.assertIn("## Tests to add", report)
        self.assertIn("(applies cleanly)", report)
        self.assertIn("2 quoting code that is not in the file", report)
        self.assertNotIn("Invented", report)
        inbox = list((self.repo / ".git/review-diff/inbox").iterdir())
        self.assertEqual(len(inbox), 1, "a copy waits for Claude Code")

        ledger = json.loads((self.repo / ".git/review-diff/ledger.json").read_text())
        bug = next(i for i, f in ledger["findings"].items() if f["kind"] == "bug")
        self.assertIn(f"(id `{bug}`)", report)
        with mock.patch("sys.stdout"):
            rd.main(["--repo", str(self.repo), "mark", bug, "fixed"])
        card = rd.score_text(json.loads((self.repo / ".git/review-diff/ledger.json").read_text()))
        self.assertIn("gemini-pro@gemini", card)
        row = next(r for r in rd.scorecard(json.loads(
            (self.repo / ".git/review-diff/ledger.json").read_text()))
            if r["reviewer"] == "gemini-pro@gemini" and r["kind"] == "bug")
        self.assertEqual((row["raised"], row["invented"], row["confirmed"], row["fixed"]),
                         (2, 1, 1, 1))

        # A mark left as a file, the way the work laptop records one
        (self.repo / ".git/review-diff/marks").mkdir()
        (self.repo / ".git/review-diff/marks" / bug).write_text("dismissed\n")
        (self.repo / ".git/review-diff/marks/nosuchid").write_text("fixed")
        rd.Run(self.repo, CFG, PROMPTS, out=self.log.append).apply_marks()
        ledger = json.loads((self.repo / ".git/review-diff/ledger.json").read_text())
        self.assertEqual(ledger["findings"][bug]["status"], "dismissed")
        self.assertEqual(list((self.repo / ".git/review-diff/marks").iterdir()), [])

        # The bookmark moved: nothing new the second time, and the turn moved on.
        self.assertIsNone(self.run_once(Fake()))
        state = json.loads((self.repo / ".git/review-diff/state.json").read_text())
        self.assertEqual(state["turn"], 1)

    def test_the_first_look_takes_turns_and_a_reviewer_out_of_quota_is_skipped(self):
        (self.repo / ".git/review-diff").mkdir(parents=True)
        (self.repo / ".git/review-diff/state.json").write_text('{"turn": 1}')
        fake = Fake(out_of_quota={"openai"})
        self.run_once(fake)
        reviews = [c[0] for c in fake.calls if c[1] == "review"]
        self.assertEqual(reviews, ["openai", "copilot", "gemini"],
                         "openai's turn, out of quota, so copilot and then gemini")
        self.assertTrue(any("openai: HTTP 429" in line for line in self.log))

    def test_a_first_run_stops_before_a_merge_that_brings_in_too_much(self):
        # main moves on by 30 commits; the branch merges main, then makes 3 of its own
        sh(self.repo, "checkout", "-qb", "dev")
        sh(self.repo, "checkout", "-q", "main")
        for i in range(30):
            (self.repo / "src/main.c").write_text(f"int main(void) {{ return {i}; }}\n")
            sh(self.repo, "commit", "-qam", f"main {i}")
        sh(self.repo, "checkout", "-q", "dev")
        sh(self.repo, "merge", "-q", "--no-ff", "--no-edit", "main")
        for i in range(3):
            (self.repo / "src/net.c").write_text(f"int net_send(int len) {{ return {i}; }}\n")
            sh(self.repo, "commit", "-qam", f"dev {i}")
        cfg = {**CFG, "first_run_commits": 20}
        rd.Run(self.repo, cfg, PROMPTS, out=self.log.append).review("HEAD", dry_run=True)
        self.assertTrue(self.log[0].startswith("3 new commits"), self.log[0])
        self.assertIn("1 files", self.log[0], "main.c came in with the merge")

    def test_the_scope_says_tests_passed_only_when_they_did(self):
        rd.Run(self.repo, CFG, PROMPTS, out=self.log.append).review("HEAD", dry_run=True)
        rd.Run(self.repo, CFG, PROMPTS, out=self.log.append).review(
            "HEAD", dry_run=True, tested=True)
        self.assertTrue(self.log[0].startswith("1 new commits"), self.log[0])
        self.assertTrue(self.log[2].startswith("Tests passed: 1 new commits"), self.log[2])

    def test_a_bare_mirror_is_reviewed_diffs_only(self):
        mirror = Path(self.tmp.name) / "little-theo-firmware.git"
        subprocess.run(["git", "clone", "-q", "--bare", str(self.repo), str(mirror)], check=True)
        fake = Fake()
        with mock.patch.object(rd, "ask", fake):
            report = rd.Run(mirror, {**CFG, "whole_files": False}, PROMPTS,
                            out=self.log.append).review("HEAD", tested=True)
        self.assertTrue((mirror / "review-diff/inbox").is_dir(), "state lives in the bare repo")
        self.assertIn("little-theo-firmware", report.read_text(encoding="utf-8"))
        review = next(c[2] for c in fake.calls if c[1] == "review")
        self.assertNotIn("full file as it is now", review)
        check = next(c[2] for c in fake.calls if c[1] == "check")
        self.assertIn("the lines around the item", check)

    def test_pieces(self):
        self.assertEqual(rd.finding_key("bug", "r", "a", "x = 1"), "08fdfd61b4b9ae76",
                         "the same id as the home system")
        self.assertEqual(rd.strip_diff_markers("+a = 1;\n b = 2;"), "a = 1;\nb = 2;")
        self.assertEqual(rd.changed_function_names(
            "@@ -1,3 +1,4 @@ int net_send(int len) {\n+static void flush_all(void) {\n"),
            ["net_send", "flush_all"])
        self.assertTrue(rd.reviewable("src/net.c"))
        self.assertFalse(rd.reviewable("build/net.c"))
        self.assertFalse(rd.reviewable("src/net.test.ts"))
        self.assertEqual(rd.parse_verdict("no, line 3 guards it")[0], "rejected")


class CallTest(unittest.TestCase):
    def test_an_api_reviewer_gets_an_openai_style_request_and_429_means_out(self):
        import http.server
        import threading
        seen = {}

        class H(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                seen.update(body=body, path=self.path, auth=self.headers["Authorization"])
                if body["model"] == "busy":
                    self.send_response(429)
                    self.end_headers()
                    return
                out = json.dumps({"choices": [{"message": {"content": "[]"}}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(out)

            def log_message(self, *a):
                pass

        srv = http.server.HTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{srv.server_port}/v1"
        r = {"name": "gemini", "base_url": base, "model": "m", "key_env": "RD_TEST_KEY"}
        with mock.patch.dict("os.environ", {"RD_TEST_KEY": "k-123"}):
            self.assertEqual(rd.ask(r, "sys", "user"), "[]")
            self.assertEqual(seen["path"], "/v1/chat/completions")
            self.assertEqual(seen["auth"], "Bearer k-123")
            self.assertEqual([m["role"] for m in seen["body"]["messages"]], ["system", "user"])
            with self.assertRaises(rd.ReviewerOut):
                rd.ask({**r, "model": "busy"}, "s", "u")
        with self.assertRaises(rd.ReviewerOut):
            rd.ask(r, "s", "u")  # no key set
        srv.shutdown()

    def test_a_command_reviewer_reads_context_md_beside_it(self):
        with tempfile.TemporaryDirectory() as d:
            tool = Path(d, "fake-copilot")
            tool.write_text("#!/bin/sh\nhead -c 9 context.md\n")
            tool.chmod(0o755)
            r = {"name": "copilot", "type": "command", "command": [str(tool), "-p", "x"]}
            self.assertEqual(rd.ask(r, "SYSTEM-IS", "user"), "SYSTEM-IS")
            with self.assertRaises(rd.ReviewerOut):
                rd.ask({**r, "command": ["no-such-copilot"]}, "s", "u")

    def test_a_command_reviewer_on_a_windows_host_gets_the_prompt_on_stdin(self):
        r = {"name": "copilot", "type": "command", "ssh_host": "beelink1",
             "command": ["copilot", "-p", "it's context.md"]}
        done = subprocess.CompletedProcess([], 0, stdout="PONG\n", stderr="")
        with mock.patch.object(rd.subprocess, "run", return_value=done) as run:
            self.assertEqual(rd.ask(r, "SYS", "user"), "PONG\n")
        argv, kw = run.call_args[0][0], run.call_args[1]
        self.assertEqual(argv[:4], ["ssh", "-o", "BatchMode=yes", "beelink1"])
        self.assertEqual(kw["input"], "SYS\n\nuser")
        script = rd.base64.b64decode(argv[-1]).decode("utf-16-le")
        self.assertIn("'-p' 'it''s context.md'", script, "quoted for PowerShell")
        self.assertIn("-CommandType Application", script, "the .cmd, not the blocked .ps1")
        unreachable = subprocess.CompletedProcess([], 255, stdout="", stderr="no route")
        with mock.patch.object(rd.subprocess, "run", return_value=unreachable):
            with self.assertRaisesRegex(rd.ReviewerOut, "can't reach beelink1"):
                rd.ask(r, "s", "u")


if __name__ == "__main__":
    unittest.main()
