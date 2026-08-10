# V1 performance evidence

`pkg-index` owns the Criterion executable; this directory owns its pinned
reference measurements and the G-PERF policy gate.

Run the macOS arm64 lane on the machine carrying the GitHub self-hosted labels
`pkg-perf-reference`, `macOS`, and `ARM64`:

```console
cargo bench --locked -p pkg-index --bench v1 -- --noplot
python3 benches/check.py --platform aarch64-darwin --runner pkg-perf-reference-m4
```

The Linux arm64 lane runs the same source and Rust release inside the pinned
container image in `.github/workflows/performance.yml`. Both baselines were
collected with native arm64 execution on the named Apple M4 reference host.
They are not portable to another CPU: the gate rejects a mismatched runner
name instead of comparing unlike hardware.

The persistent reference host never runs pull-request revisions. Its workflow
runs only after code reaches `main`, including an explicit maintainer dispatch
of `main`; non-main dispatches are rejected and checkout is fixed to `main`.
Pull-request correctness remains covered by hermetic tests and review;
G-PERF is a post-merge/release gate so untrusted repository code cannot use the
self-hosted runner or its Docker daemon.

Every measurement must remain under both its accepted absolute V1 ceiling and
125% of the pinned median. Updating a baseline requires reviewer sign-off and
an honest replacement of its provenance; QEMU measurements are diagnostic and
must never be committed as release evidence.

`real-nix-budgets.json` records the accepted Real Nix ceilings but deliberately
marks them pending. The PR-36 privileged Real-Nix lane supplies those results;
these in-process fixtures cannot satisfy them. Native x86_64 reference
baselines are also required before GA and remain pending.
