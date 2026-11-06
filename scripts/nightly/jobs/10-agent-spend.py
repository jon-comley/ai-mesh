#!/usr/bin/env python3
"""Copy each agent's spend per day into the "Agent spend by day" sheet in Drive
(Records), for metrics later.

Reads the day files agentterm keeps under ~/.ai-mesh/agents/term/<agent>/costs/
(dollars, never deleted). Adds a row for each agent and day the sheet lacks, and
corrects any row whose dollars changed: a day added while it was still running
is put right the night after. Today is left out until it is over.

Writes as a Google service account whose key is AGENT_SPEND_KEY (default
~/.ai-mesh/agents/keys/nightly-sa.json); the sheet is shared with it as an
editor. Standard library plus `cryptography`, already on lenovo1.
"""

import base64
import datetime as dt
import json
import os
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding

SHEET_ID = "1HaOUg03itK6gTTQaL_B36qA-0alKcHs_wl54N-M4dhc"
TERM_DIR = Path.home() / ".ai-mesh/agents/term"
KEY_FILE = Path(os.environ.get("AGENT_SPEND_KEY", Path.home() / ".ai-mesh/agents/keys/nightly-sa.json"))
API = f"https://sheets.googleapis.com/v4/spreadsheets/{SHEET_ID}"


def b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def token(key: dict) -> str:
    now = int(time.time())
    head = b64(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    claims = b64(json.dumps({
        "iss": key["client_email"],
        "scope": "https://www.googleapis.com/auth/spreadsheets",
        "aud": key["token_uri"],
        "iat": now,
        "exp": now + 600,
    }).encode())
    signer = serialization.load_pem_private_key(key["private_key"].encode(), password=None)
    sig = signer.sign(f"{head}.{claims}".encode(), padding.PKCS1v15(), hashes.SHA256())
    body = urllib.parse.urlencode({
        "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
        "assertion": f"{head}.{claims}.{b64(sig)}",
    }).encode()
    with urllib.request.urlopen(key["token_uri"], body, timeout=20) as r:
        return json.load(r)["access_token"]


def call(tok: str, method: str, url: str, body=None) -> dict:
    req = urllib.request.Request(url, method=method, data=json.dumps(body).encode() if body else None,
                                 headers={"Authorization": f"Bearer {tok}", "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def local_days(today: str) -> dict:
    """(date, agent) -> dollars, for every finished day."""
    out = {}
    for costs in sorted(TERM_DIR.glob("*/costs")):
        agent = costs.parent.name
        for f in costs.iterdir():
            try:
                dt.date.fromisoformat(f.name)
            except ValueError:
                continue
            if f.name < today:
                out[(f.name, agent)] = round(float(f.read_text().strip() or 0), 6)
    return out


def plan(sheet_rows: list, local: dict) -> tuple:
    """The cell updates and new rows that bring the sheet up to `local`."""
    have = {}
    for i, row in enumerate(sheet_rows[1:], start=2):
        if len(row) >= 2:
            have[(row[0], row[1])] = (i, float(row[2]) if len(row) > 2 and row[2] != "" else None)
    fixes = [(f"Daily!C{i}", local[k]) for k, (i, usd) in have.items()
             if k in local and (usd is None or abs(usd - local[k]) > 1e-6)]
    new = sorted(k for k in local if k not in have)
    first = max(len(sheet_rows), 1) + 1
    rows = [[d, a, local[(d, a)], f"=C{n}*Settings!$B$2", f"=B{n}<>Settings!$B$3"]
            for n, (d, a) in enumerate(new, start=first)]
    return fixes, rows, first


def main() -> int:
    today = dt.date.today().isoformat()
    local = local_days(today)
    tok = token(json.loads(KEY_FILE.read_text()))
    # Unformatted, so dollars come back as numbers and dates as typed.
    got = call(tok, "GET", f"{API}/values/Daily!A:C?valueRenderOption=UNFORMATTED_VALUE&dateTimeRenderOption=FORMATTED_STRING")
    fixes, rows, first = plan(got.get("values", []), local)
    data = [{"range": r, "values": [[v]]} for r, v in fixes]
    if rows:
        data.append({"range": f"Daily!A{first}:E{first + len(rows) - 1}", "values": rows})
    if data:
        call(tok, "POST", f"{API}/values:batchUpdate", {"valueInputOption": "USER_ENTERED", "data": data})
    print(f"agent spend: {len(rows)} rows added, {len(fixes)} corrected, up to {today} (not included)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
