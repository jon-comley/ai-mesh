#!/usr/bin/env python3
"""Save an online AI provider's key on pi1 from the terminal, as the Online AI
tab's Save does, then test it, without changing the provider chat uses.

    scripts/set-ai-key.py workersai     (or nvidia, groq, gemini, openrouter...)

The key is typed at a hidden prompt. The mesh token is read from pi1 over ssh.
"""
import getpass
import json
import subprocess
import sys
import urllib.request

API = "http://pi1:9001/api/gateway"


def call(token, path="", body=None):
    req = urllib.request.Request(
        API + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="GET" if body is None else "POST",
    )
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    token = subprocess.run(
        ["ssh", "pi1", "sudo sed -n 's/^MESH_AUTH_TOKEN=//p' /var/lib/ai-mesh/coordinator.state"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    before = call(token)
    preset = next((p for p in before["presets"] if p["id"] == sys.argv[1]), None)
    if preset is None:
        sys.exit("no preset " + sys.argv[1] + "; one of: " + ", ".join(p["id"] for p in before["presets"]))
    key = getpass.getpass(f"{preset['label']} key: ").strip()
    if not key:
        sys.exit("no key typed, nothing saved")
    try:
        # A key is stored against the endpoint in effect, so switch to it, save, test.
        call(token, body={"base_url": preset["base_url"], "selected_model": preset["models"][0], "api_key": key})
        print(json.dumps(call(token, "/test", {})))
    finally:
        call(token, body={"base_url": before["base_url"], "selected_model": before["selected_model"]})
        print("chat provider left as", before["base_url"], before["selected_model"])


if __name__ == "__main__":
    main()
