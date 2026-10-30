#!/usr/bin/env python3
"""Stop pkg profile changes at random points and check the profile after each stop.

The test runs a compiled client against real Nix in a disposable state folder,
like tools/native-cli-check.py. A local flake has two packages, `probe` and
`spare`. Each trial starts one change: install, remove, upgrade, or rollback.
It stops the change with a signal at a random time, injects one failed system
call into the pkg process with strace, or runs several changes at the same
time. Then it checks that:

- `pkg list` works, and its entries match the commands in the profile
- the profile has the packages from before or after the change, or from an
  older generation
- the same change, run again, finishes with the expected packages
- no temporary files stay

It does not change the user's profile. Run it on a Linux machine with Nix,
strace, and a C compiler:

    python3 tools/stress/lifecycle.py --pkg target/release/pkg
"""

import argparse
import collections
import contextlib
import json
import math
import os
import random
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SYSTEM = "x86_64-linux"
BUILDER = r"""
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
int main(int argc, char **argv) {
    const char *out = getenv("out");
    if (!out) { puts(VERSION); return 0; }
    if (argc != 3) return 2;
    char bin[4096], dst[4096];
    snprintf(bin, sizeof bin, "%s/bin", out);
    if (mkdir(out, 0755) || mkdir(bin, 0755)) return 3;
    snprintf(dst, sizeof dst, "%s/%s", bin, argv[2]);
    FILE *in = fopen(argv[1], "rb"), *to = fopen(dst, "wb");
    if (!in || !to) return 4;
    char buffer[8192]; size_t n;
    while ((n = fread(buffer, 1, sizeof buffer, in)))
        if (fwrite(buffer, 1, n, to) != n) return 5;
    if (fclose(to) || chmod(dst, 0755)) return 6;
    return 0;
}
"""
INJECT = [
    ("openat", "EACCES"), ("openat", "EMFILE"), ("read", "EIO"), ("read", "EINTR"),
    ("write", "ENOSPC"), ("write", "EINTR"), ("flock", "EINTR"), ("flock", "ENOLCK"),
    ("fsync", "EIO"), ("renameat2", "EIO"), ("unlinkat", "EBUSY"), ("mkdir", "EACCES"),
    ("clone3", "EAGAIN"), ("pipe2", "EMFILE"), ("wait4", "EINTR"), ("poll", "EINTR"),
    ("readlink", "EIO"), ("getdents64", "EIO"), ("statx", "EIO"),
]
# Not `getrandom`: std asks with GRND_INSECURE, which the kernel never fails,
# and std panics when it fails.
ROOT = PKG = PROFILE = SOURCE = None
ENV = {}
# The packages of every generation that may exist: (names, probe version).
KNOWN = set()
# The measured time of each change, in seconds.
DURATION = {}
FLAKE = {"version": "1", "nonce": 0}


def setup(pkg, root):
    global ROOT, PKG, PROFILE, SOURCE
    ROOT, PKG = root, str(pkg)
    PROFILE = root / "state/nix/profiles/pkg"
    SOURCE = root / "source"
    SOURCE.mkdir(parents=True)
    (root / "home").mkdir()
    ENV.update(os.environ, HOME=str(root / "home"), XDG_STATE_HOME=str(root / "state"),
               XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"),
               NO_COLOR="1")
    (SOURCE / "builder.c").write_text(BUILDER)
    for version in ("1", "2"):
        subprocess.run(["cc", "-static", f'-DVERSION="{version}"', SOURCE / "builder.c",
                        "-o", SOURCE / f"builder{version}"], check=True)
    config = root / "config/pkg/config.toml"
    config.parent.mkdir(parents=True)
    reference = json.dumps(f"path:{SOURCE}")
    config.write_text(f"[sources]\nnixpkgs = {reference}\ncasks = {reference}\n")
    flake("1")


def flake(version):
    """Write the flake. A new nonce gives new store paths, so Nix builds again."""
    FLAKE["version"] = version
    FLAKE["nonce"] += 1

    def package(name):
        return (f'{name} = builtins.derivation {{ name = "{name}-{version}"; '
                f'system = "{SYSTEM}"; builder = ./builder{version}; '
                f'args = [ ./builder{version} "{name}" ]; nonce = "{ROOT.name}-{FLAKE["nonce"]}"; }} '
                f'// {{ version = "{version}"; pname = "{name}"; '
                'meta.description = "pkg lifecycle stress fixture"; };')

    (SOURCE / "flake.nix").write_text(
        f'{{ outputs = {{self}}: {{ packages.{SYSTEM} = {{ {package("probe")} '
        f'{package("spare")} }}; }}; }}')


# ---------------------------------------------------------------- running


def start(*args, strace=None):
    command = [PKG, *args]
    if strace:
        command = ["strace", "-qq", "-o", "/dev/null", *strace, "--", *command]
    return subprocess.Popen(command, env=ENV, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, start_new_session=True)


def finish(process, timeout):
    try:
        out, err = process.communicate(timeout=timeout)
        return process.returncode, out, err
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        out, err = process.communicate()
        return "hang", out, err


def run(*args, timeout=300):
    return finish(start(*args), timeout)


def last(err):
    lines = [line for line in err.splitlines() if line.strip()]
    panic = next((i for i, line in enumerate(lines) if " panicked at " in line), None)
    if panic is not None:
        return " ".join(lines[panic : panic + 2])[:200]
    return next((line for line in lines if line.startswith(("error", "Error"))),
                lines[-1] if lines else "")[:200]


def observe():
    """The packages that `pkg list` reports, and the commands in the profile."""
    code, out, err = run("list", "--json", timeout=120)
    if code != 0:
        return {"list": f"exit {code}: {last(err)}"}
    names = sorted(entry["name"] for entry in json.loads(out)["result"])
    commands = {}
    for name in ("probe", "spare"):
        path = PROFILE / "bin" / name
        if path.exists():
            result = subprocess.run([path], capture_output=True, text=True, timeout=10, env={})
            commands[name] = result.stdout.strip() if result.returncode == 0 else "broken"
    return {"names": names, "commands": commands}


def key(state):
    return (tuple(state["names"]), state["commands"].get("probe"))


def problems_of(state):
    if "list" in state:
        return [f"pkg list failed: {state['list']}"]
    problems = []
    if sorted(state["names"]) != sorted(state["commands"]):
        problems.append(f"entries {state['names']} do not match commands {sorted(state['commands'])}")
    if "broken" in state["commands"].values():
        problems.append("a profile command does not start")
    return problems


def leftovers():
    found = []
    for base in ("state", "cache", "config", "home"):
        for path in (ROOT / base).rglob("*"):
            if path.name.startswith((".tmp", ".pkg-")) or path.name.endswith((".tmp", ".partial")):
                found.append(str(path.relative_to(ROOT)))
    return sorted(found)


# ---------------------------------------------------------------- changes


def change(operation, before):
    """The arguments of one change and the packages it must give."""
    names, version = key(before)
    if operation == "install":
        return ["install", "nixpkgs:spare"], (tuple(sorted({*names, "spare"})), version)
    if operation == "remove":
        return ["remove", "spare"], (tuple(n for n in names if n != "spare"), version)
    if operation == "upgrade":
        flake("2" if version == "1" else "1")
        return ["upgrade", "--all"], (names, FLAKE["version"])
    return ["rollback"], None


def choose(state):
    return random.choice(["upgrade", "rollback",
                          "remove" if "spare" in state["names"] else "install"])


def trial(report, label, operation, *, signal_number=None, strace=None, fault=None):
    before = observe()
    assert not problems_of(before), before
    args, expected = change(operation, before)
    if expected:
        KNOWN.add(expected)
    allowed = {key(before), expected} if expected else KNOWN | {key(before)}
    delay = round(random.uniform(0, DURATION[operation] * 1.1), 3) if signal_number else None
    began = time.monotonic()
    process = start(*args, strace=strace)
    if signal_number:
        time.sleep(delay)
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal_number)
    code, out, err = finish(process, 300)
    elapsed = time.monotonic() - began
    state = observe()
    problems = problems_of(state)
    if code == "hang":
        problems.append("the change did not stop")
    if "panicked" in err or code == 101:
        problems.append("panic")
    if not problems and key(state) not in allowed:
        problems.append(f"unexpected packages {key(state)}")
    if code == 0 and expected and not problems and key(state) != expected:
        problems.append("success without the change")
    if code == 0 and not signal_number and not strace:
        DURATION[operation] = 0.7 * DURATION.get(operation, elapsed) + 0.3 * elapsed
    retry = None
    # The same change, run again, must finish the job.
    if expected and not problems and key(state) != expected:
        retry_code, _, retry_err = run(*args)
        state = observe()
        retry = retry_code
        if retry_code != 0 or problems_of(state) or key(state) != expected:
            problems.append(f"retry: exit {retry_code}: {last(retry_err)}")
    if "list" not in state:
        KNOWN.add(key(state))
    files = leftovers()
    if files:
        problems.append(f"leftovers {files}")
    report({"scenario": label, "operation": operation, "fault": fault, "delay": delay,
            "code": code, "retry": retry, "state": key(state) if "list" not in state else None,
            "message": last(err), "problems": problems})
    return state


# ---------------------------------------------------------------- parts


def baseline(report):
    """Install probe, then run each change once to measure it."""
    code, _, err = run("install", "nixpkgs:probe")
    assert code == 0, err
    KNOWN.add(key(observe()))
    for operation in ("install", "upgrade", "remove", "upgrade", "rollback", "rollback"):
        trial(report, "baseline", operation)
    print("durations:", {name: round(value, 2) for name, value in DURATION.items()}, flush=True)


def kills(report, rounds):
    signals = [signal.SIGKILL] * 3 + [signal.SIGINT, signal.SIGTERM]
    state = observe()
    for _ in range(rounds):
        number = random.choice(signals)
        operation = choose(state)
        state = trial(report, f"kill {number.name}", operation, signal_number=number)


def positions(count, limit):
    if count <= limit:
        return list(range(1, count + 1))
    head = list(range(1, limit // 2 + 1))
    steps = max(2, limit - len(head))
    tail = {round(math.exp(math.log(len(head) + 1)
                           + i * (math.log(count) - math.log(len(head) + 1)) / (steps - 1)))
            for i in range(steps)}
    return sorted(set(head) | tail)


def ensure(present):
    if ("spare" in observe()["names"]) != present:
        code, _, err = run("install", "nixpkgs:spare") if present else run("remove", "spare")
        assert code == 0, err


def faults(report, limit):
    """Fail one system call of the pkg process. Nix itself is not traced."""
    for operation in ("install", "remove"):
        ensure(operation == "remove")
        trace = ROOT / "trace"
        names = sorted({name for name, _ in INJECT})
        args, _ = change(operation, observe())
        finish(start(*args, strace=["-o", str(trace), "-e", f"trace={','.join(names)}"]), 300)
        counts = collections.Counter(match.group(1) for line in trace.read_text().splitlines()
                                     if (match := re.match(r"(\w+)\(", line)))
        print(f"{operation} calls:", dict(sorted(counts.items())), flush=True)
        for name, error in INJECT:
            for when in positions(counts[name], limit):
                ensure(operation == "remove")
                strace = ["-e", f"trace={name}", "-e", f"inject={name}:error={error}:when={when}"]
                trial(report, f"fault {operation}", operation, strace=strace,
                      fault=f"{name}#{when}={error}")


def parallel(report, rounds):
    """Start the same change four times at once. The lock must keep one entry."""
    for _ in range(rounds):
        for operation in ("install", "remove"):
            ensure(operation == "remove")
            args, expected = change(operation, observe())
            results = [finish(process, 300) for process in [start(*args) for _ in range(4)]]
            state = observe()
            problems = problems_of(state)
            codes = sorted(str(code) for code, _, _ in results)
            if "list" not in state and key(state) != expected:
                problems.append(f"unexpected packages {key(state)}")
            if "0" not in codes or "hang" in codes:
                problems.append(f"exit codes {codes}")
            if any("panicked" in err for _, _, err in results):
                problems.append("panic")
            report({"scenario": f"parallel {operation} x4", "operation": operation,
                    "code": codes, "state": key(state) if "list" not in state else None,
                    "message": next((last(err) for code, _, err in results if code), ""),
                    "problems": problems + ([f"leftovers {leftovers()}"] if leftovers() else [])})


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("parts", nargs="*", default=["kill", "fault", "parallel"])
    parser.add_argument("--pkg", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=150, help="signal trials")
    parser.add_argument("--positions", type=int, default=8, help="fault positions per call")
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--root", type=Path, help="keep the state in this new folder")
    arguments = parser.parse_args()
    assert shutil.which("nix") and shutil.which("strace"), "Nix and strace are required"
    random.seed(arguments.seed)
    root = arguments.root or Path(tempfile.mkdtemp(prefix="pkg-lifecycle-"))
    root.mkdir(parents=True, exist_ok=True)
    print(f"root: {root}", flush=True)
    setup(arguments.pkg.resolve(strict=True), root)
    results = []
    log = (root / "results.jsonl").open("w")

    def report(result):
        results.append(result)
        log.write(json.dumps(result) + "\n")
        log.flush()
        flag = "BAD " if result["problems"] else ""
        print(f"{flag}{result['scenario']}: {result['operation']} {result.get('fault') or ''} "
              f"code={result['code']} state={result['state']} {result['message']} "
              f"{result['problems'] or ''}", flush=True)

    baseline(report)
    for part in arguments.parts:
        {"kill": lambda: kills(report, arguments.rounds),
         "fault": lambda: faults(report, arguments.positions),
         "parallel": lambda: parallel(report, 5)}[part]()
    groups = collections.Counter((r["scenario"], bool(r["problems"])) for r in results)
    print("\nscenario | bad | trials")
    for (scenario, broken), number in sorted(groups.items()):
        print(f"{scenario} | {'BAD' if broken else '-'} | {number}")
    sys.exit(1 if any(r["problems"] for r in results) else 0)


if __name__ == "__main__":
    main()
