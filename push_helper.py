#!/usr/bin/env python3
"""One-shot helper: push /data/github/local-compact to the private GitHub repo
using GITHUB_PAT from /data/kb/.env. Never prints the token."""
import os
import subprocess
import sys

DIR = "/data/github/local-compact"
REPO = "handtrixx/local-compact"


def token():
    with open("/data/kb/.env") as f:
        for line in f:
            if line.startswith("GITHUB_PAT="):
                return line.split("=", 1)[1].strip()
    sys.exit("GITHUB_PAT not found in /data/kb/.env")


def run(cmd):
    p = subprocess.run(cmd, cwd=DIR, capture_output=True, text=True)
    if p.returncode != 0:
        print("FAILED:", " ".join(cmd))
        print(p.stderr)
        sys.exit(1)
    return p.stdout


t = token()
if not os.path.isdir(DIR + "/.git"):
    run(["git", "init", "-b", "main"])
run(["git", "config", "user.name", "Niklas Stephan"])
run(["git", "config", "user.email", "pooh85@gmx.de"])
run(["git", "add", "-A"])
run(["git", "commit", "-m", "Initial PoC: deterministic fake-LLM context compactor"])
url = "https://oauth2:%s@github.com/%s.git" % (t, REPO)
p = subprocess.run(
    ["git", "push", url, "main"], cwd=DIR, capture_output=True, text=True,
    env={**os.environ, "GIT_TERMINAL_PROMPT": "0"},
)
if p.returncode != 0:
    print("push failed (token hidden):", p.stderr.replace(t, "***"))
    sys.exit(1)
print("pushed to https://github.com/%s (private)" % REPO)
