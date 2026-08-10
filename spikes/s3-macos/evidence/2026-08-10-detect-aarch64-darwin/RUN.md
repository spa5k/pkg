# Reviewed run context

- Date: 2026-08-10 (Asia/Kolkata).
- Host: macOS 26.6 (build 25G72), native arm64 / `aarch64-darwin`.
- Harness: reviewed `s3-probe` CLI built from this tree with Rust 1.96.1;
  production `BoundedProbeRunner`, not an injected library runner.
- Nix path supplied for existence-only Detect:
  `/nix/var/nix/profiles/default/bin/nix`; separately observed as Nix 2.34.8.
- Command:

  ```text
  ./target/release/s3-probe detect \
    --nix-bin /nix/var/nix/profiles/default/bin/nix \
    --out-dir target/s3-detect-real-with-nix
  ```

- Exit: 0. The generated report was re-read and reviewed, then copied here with
  whitespace normalization only; a canonical `jq -S` comparison against the
  generated report was byte-equal. `summary.md` is a curated faithful summary,
  not a claim that the generated Markdown bytes were preserved.
- The product-managed daemon socket parent was independently observed as
  `root:pkg-nix-broker` `0750`; this correctly prevents the console user from
  running the Preflight directly. A separate attempted Preflight verified the
  exact Nix version and then stopped Incomplete before prefetch with a closed
  `Unknown` failure because the invoking console user could not traverse the
  daemon socket. That Incomplete run is diagnostic only and is not committed as
  evidence.
- The zero identity counts are valid negative capability evidence. They keep
  real Developer-ID signing/notarization Pending; no credentials were requested,
  read, written, or logged.
