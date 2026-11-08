#!/usr/bin/env python3
"""Dated reminders, pushed to Jon's phone through ntfy (8 Nov 2026).

Each line of reminders.txt (beside this, not executable, so nightly.sh skips it) is `YYYY-MM-DD text`. On or after its date, a
reminder is pushed once, to the topic the eBay alerts use (the coordinator's
`__ebay__ ntfy_topic_url` preference), and remembered in
~/.ai-mesh/reminders-sent so it is not pushed again.
"""
import datetime
import os
import sqlite3
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REMINDERS = os.path.join(HERE, 'reminders.txt')
SENT = os.path.expanduser('~/.ai-mesh/reminders-sent')
DB = 'file:/var/lib/ai-mesh/ai_mesh.db?mode=ro'


def topic() -> str:
    row = sqlite3.connect(DB, uri=True).execute(
        "select value from dashboard_preferences where user_id = '__ebay__' and key = 'ntfy_topic_url'"
    ).fetchone()
    if not row:
        sys.exit('no ntfy topic in the coordinator preferences')
    return row[0]


def main() -> None:
    today = datetime.date.today().isoformat()
    sent = set(open(SENT).read().splitlines()) if os.path.exists(SENT) else set()
    due = []
    for line in open(REMINDERS):
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        date, _, text = line.partition(' ')
        if date <= today and line not in sent:
            due.append(line)
    if not due:
        print('nothing due')
        return
    url = topic()
    for line in due:
        req = urllib.request.Request(url, data=line.partition(' ')[2].encode(), headers={'Title': 'Reminder'})
        urllib.request.urlopen(req, timeout=20).read()
        with open(SENT, 'a') as f:
            f.write(line + '\n')
        print('pushed:', line)


main()
