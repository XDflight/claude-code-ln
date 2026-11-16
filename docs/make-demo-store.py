#!/usr/bin/env python3
"""Builds a fictional session store for screenshots (no real data involved).

Usage: make-demo-store.py <empty dir> [en|zh]
Then:  HOME=<dir>/home claude-code-ln --root <dir>/home/Claude/claude-code-sessions \
           --projects <dir>/home/.claude/projects
"""
import json, os, sys, time, uuid

base = os.path.abspath(sys.argv[1])
lang = sys.argv[2] if len(sys.argv) > 2 else "en"
home = os.path.join(base, "home")
root = os.path.join(home, "Claude", "claude-code-sessions")
projects = os.path.join(home, ".claude", "projects")
now = int(time.time() * 1000)
H = 3600 * 1000

def uid(name):
    return str(uuid.uuid5(uuid.NAMESPACE_DNS, "demo." + name))

accounts = {"work": "alex@acme.example", "home": "alex@example.org"}
orgs = {"acme": "Acme Labs", "oss": "Open Source Guild", "personal": "Personal", "studio": "Nimbus Studio"}
layout = [("work", "acme"), ("work", "oss"), ("home", "personal"), ("home", "studio")]
for a, o in layout:
    os.makedirs(os.path.join(root, uid(a), uid(o)), exist_ok=True)

def record(name, title, cwd, cli, age_h, archived=False):
    return {
        "sessionId": "local_" + uid(name), "cliSessionId": uid(cli),
        "cwd": os.path.join(home, "Projects", cwd), "title": title, "model": "claude-opus-5-5",
        "createdAt": now - 200 * H, "lastActivityAt": now - int(age_h * H), "isArchived": archived,
    }

def put(org, name, title, cwd, cli, age_h, **kw):
    a = next(a for a, o in layout if o == org)
    path = os.path.join(root, uid(a), uid(org), "local_%s.json" % uid(name))
    with open(path, "w") as f:
        json.dump(record(name, title, cwd, cli, age_h, **kw), f)
    t = os.path.join(projects, "-demo-" + cwd.replace("/", "-"))
    os.makedirs(t, exist_ok=True)
    with open(os.path.join(t, uid(cli) + ".jsonl"), "w") as f:
        f.write("{}\n" * (4000 + 900 * len(title)))

# Linked and in sync across three organizations.
for o in ("acme", "oss", "personal"):
    put(o, "s1", "Rewrite the tile renderer in Rust", "atlas/renderer", "t1", 0.4)
# Linked; one copy's metadata is behind.
put("acme", "s2", "Flaky integration test triage", "atlas/api", "t2", 2)
put("studio", "s2", "Flaky integration test triage", "atlas/api", "t2", 26)
# Forked: the two copies continued into different transcripts.
put("oss", "s3", "Design doc: offline sync protocol", "notes/sync-rfc", "t3a", 5)
put("personal", "s3", "Design doc: offline sync protocol", "notes/sync-rfc", "t3b", 31)
# Only in one organization.
put("acme", "s4", "Migrate CI from Jenkins to Buildkite", "infra/ci", "t4", 9)
put("personal", "s5", "Weekend project: e-ink weather station", "hobby/weather-station", "t5", 20)
put("oss", "s6", "Fix Unicode width bug in table layout", "oss/tabulate-rs", "t6", 44)
put("studio", "s7", "Landing page copy and hero animation", "studio/landing", "t7", 52)
for o in ("acme", "studio"):
    put(o, "s8", "Benchmark: Postgres vs. SQLite for sync log", "atlas/bench", "t8", 70)
put("oss", "s9", "Old spike: WASM plugin sandbox", "oss/sandbox", "t9", 300, archived=True)
# A tombstone: s4 was once deleted in "Personal".
open(os.path.join(root, uid("home"), uid("personal"), "deleted_" + uid("s4")), "w").write(str(now))

cfg = os.path.join(home, "Library", "Application Support", "claude-code-ln")
os.makedirs(cfg, exist_ok=True)
detected = {uid(k): v for k, v in {**accounts, **orgs}.items()}
json.dump({"language": lang, "detected": detected}, open(os.path.join(cfg, "config.json"), "w"))
print(root)
