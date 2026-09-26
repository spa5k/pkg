#!/usr/bin/env python3
import datetime, json, pathlib, platform, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parent
result = {"startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(), "platform": platform.platform(), "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(), "runs": []}
with tempfile.TemporaryDirectory(prefix="pkg-lease-review-run-") as temporary:
    tmp = pathlib.Path(temporary)
    for name in ("fork", "spawn"):
        compiled = subprocess.run(["rustc", "--edition=2024", str(root / (name + ".rs")), "-o", str(tmp / name)], capture_output=True, text=True)
        if compiled.returncode:
            raise RuntimeError(compiled.stderr)
        run = subprocess.run([str(tmp / name), str(tmp / (name + ".lease"))], capture_output=True, text=True, timeout=10)
        (root / (name + ".log")).write_text(run.stdout + run.stderr)
        result["runs"].append({"name": name, "exitCode": run.returncode, "stdout": run.stdout, "stderr": run.stderr})
        print(name + ": " + run.stdout.strip(), flush=True)
        run.check_returncode()
(root / "results.json").write_text(json.dumps(result, indent=2) + "\n")
