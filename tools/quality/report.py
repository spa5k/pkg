#!/usr/bin/env python3
"""Turns clippy JSON output into one compact diagnostic object per line."""
import json
import sys

root = sys.argv[1]
for line in sys.stdin:
    try:
        m = json.loads(line)
    except json.JSONDecodeError:
        continue
    if m.get("reason") != "compiler-message":
        continue
    msg = m["message"]
    code = msg.get("code")
    if not isinstance(code, dict):
        continue
    span = next((s for s in msg.get("spans", []) if s.get("is_primary")), None)
    if span is None:
        continue
    file_name = span.get("file_name", "")
    rel = file_name[len(root) + 1:] if file_name.startswith(root + "/") else file_name
    print(json.dumps({
        "lint": code.get("code"),
        "file": rel,
        "line": span.get("line_start"),
        "message": msg.get("message", ""),
    }))
