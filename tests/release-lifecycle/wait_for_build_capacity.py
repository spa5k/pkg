#!/usr/bin/env python3
"""Wait for spare CPU capacity in the disposable hosted macOS build fixture."""
import math
import os
import platform
import subprocess
import time


def wait_for_capacity(cores, sample, now, sleep, emit, timeout=600):
    deadline = now() + timeout
    consecutive = 0
    while True:
        load = sample()
        if not math.isfinite(load) or load < 0:
            raise ValueError(f"invalid one-minute CPU load: {load!r}")
        consecutive = consecutive + 1 if load <= cores else 0
        emit(f"one-minute load={load:.2f}; logical CPUs={cores}; ready samples={consecutive}/3")
        if consecutive == 3:
            return
        remaining = deadline - now()
        if remaining <= 0:
            raise TimeoutError('hosted runner did not provide spare build capacity within the fixture deadline')
        sleep(min(15, remaining))


def main():
    if (platform.system() != 'Darwin' or os.environ.get('GITHUB_ACTIONS') != 'true'
            or os.environ.get('RUNNER_ENVIRONMENT') != 'github-hosted'):
        raise SystemExit('requires a disposable GitHub-hosted macOS runner')
    cores = int(subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.logicalcpu'], text=True))
    if cores < 1:
        raise SystemExit('invalid logical CPU count')
    wait_for_capacity(cores, lambda: os.getloadavg()[0], time.monotonic, time.sleep,
                      lambda line: print(line, flush=True))
    print('PASS: hosted fixture has spare capacity; product admission checks remain enabled', flush=True)


if __name__ == '__main__':
    main()
