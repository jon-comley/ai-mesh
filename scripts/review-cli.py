#!/usr/bin/env python3
"""Helpers for `just review-now` and `just ask` (docs/code-review.md).

    review-cli.py run-body <repo> <mode>     JSON body for /api/reviews/run-now
    review-cli.py ask-body <repo> <question> JSON body for /api/reviews/ask
    review-cli.py answer <question-id>       read GET /api/reviews on stdin; print
                                             the answer if there is one yet
"""
import json
import sys


def run_body(repo, mode):
    body = {"repo": repo, "sweep": mode == "sweep"}
    if mode.startswith("path:"):
        body["path"] = mode[len("path:"):]
    elif mode.startswith("branch:"):
        body["branch"] = mode[len("branch:"):]
    elif mode not in ("", "sweep"):
        sys.exit("mode must be sweep, path:<folder-or-file> or branch:<name>")
    return body


def answer(qid, view):
    snap = view.get("snapshot") or {}
    q = next((q for q in snap.get("questions", []) if q.get("id") == qid), None)
    if not q:
        return None
    if q.get("status") == "answered":
        sources = ", ".join(q.get("sources", [])) or "nothing"
        return f"{q.get('answer', '')}\n\n— {q.get('worker') or 'no model needed'}; read: {sources}"
    if q.get("status") == "failed":
        return "✗ " + (q.get("error") or "failed")
    return None


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd = argv[1]
    if cmd == "run-body" and len(argv) == 4:
        print(json.dumps(run_body(argv[2], argv[3])))
    elif cmd == "ask-body" and len(argv) == 4:
        print(json.dumps({"repo": argv[2], "question": argv[3]}))
    elif cmd == "answer" and len(argv) == 3:
        out = answer(argv[2], json.load(sys.stdin))
        if out:
            print(out)
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
