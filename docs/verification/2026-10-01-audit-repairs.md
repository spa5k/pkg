# Accepted audit repairs: verification

Date: 1 October 2026. Change:
[implement-audit-repairs](../../openspec/changes/implement-audit-repairs/proposal.md).
Baseline: `120e9edd2730f647c6e1302bfaef4a5118030d5e`.
Tested code: `de3f1daec90f1ae5e6347a570160750d2061ca76`.
The later completion record changes documentation only.

GLM-5.3 prepared the first implementation in E2B. The parent took over
after the quality step. Three read-only reviews covered the client,
catalog pipeline, and delivery checks. The parent then fixed child
reaping, config metadata error handling, publication/store binding,
and the environment tests. No recursive delegation was used.

## Hosts

| Host | Tools |
| --- | --- |
| Apple silicon, macOS 27.0.1 | Rust 1.96.1, ShellCheck 0.11.0, cargo-deny 0.20.2 |
| E2B, x86_64 Linux 6.1.158+ | Rust 1.96.1, ShellCheck 0.11.0, cargo-deny 0.20.2, Determinate Nix 3.22.5 / Nix 2.35.2 |

The macOS checks used a temporary clone. The original checkout stayed
clean at the baseline. The Linux checks used the final parent code,
not the earlier GLM branch heads.

## Results

Every command below returned exit status 0 on both hosts:

```sh
cargo build --locked --all-targets
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items
sh -n install.sh
shellcheck install.sh
python3 docs/tests/test_install_script.py
python3 .github/scripts/check_docs_links.py
cargo deny --locked --offline check bans licenses sources
```

The workspace ran 253 tests on each host. Nested test-process runs are
not counted twice. The installer suite ran 43 tests on Linux. It ran 43
tests with seven platform skips on macOS. Clippy checked both workspace
crates and all targets on macOS, including the macOS-only client code.

These Linux commands also returned exit status 0:

```sh
cargo build --locked --release -p pkg-cli
python3 tools/native-cli-check.py --pkg target/release/pkg
```

The native check passed lifecycle operations, mutable source handling,
namespace choices, tap isolation, and cancellation in disposable E2B
state. The macOS native check was left to CI because that script keeps
the real account HOME and uses its Applications directory. No package
operation or administrator setup ran on the user's account.

## Catalog bytes

The parent regenerated the catalog from the pinned snapshot in E2B.
The snapshot hash matched `nix/casks/catalog/input.json`:

`1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251`.

The generated and committed `catalog.json` files were byte-identical:

`e069bfcc34f5bfb30751df48c85bb7549ef9fd7968d7d4afa181d309e2722771`.

Schema, eligibility rules, and generated consumers stayed unchanged.
This check proves the catalog contract. It does not prove that every
eligible package works.

## Focused regression coverage

- Failed setup input stops and reaps the child.
- A metadata error or non-file cannot become a missing global config.
- Failed sandbox queries retain their cause in verification and doctor.
- Typed importer refusal triggers the setup path; matching ordinary error
  text does not.
- First and replacing publication rollback use the originating locked
  store and leave another state root intact.
- Foreign staged generations are refused. An abandoned publication
  retains the old generation and scratch without fallible rollback.
- Both Nix child tests carry only controlled sentinels. They check keys
  before values and account for shell bookkeeping on Linux and macOS.

The snapshot-loader proposal was rejected after comparison. The existing
cache module already provides shared persistence. A new loader would
need extra callbacks while hiding different lane behavior.

Full command logs and the Git bundle were saved outside the repository.
