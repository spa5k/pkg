# Fuzz targets

These targets feed random bytes to the parsers that read data from outside pkg.
Each target also checks the result, so a wrong accept is a failure too.

| Target | Input | Check |
| --- | --- | --- |
| `release_signature` | `SHA256SUMS` text and an SSH signature | Only the test key verifies. |
| `release_sums` | `SHA256SUMS` text | The archive name, checksum, and version are canonical. |
| `release_archive` | A release archive | The client is not empty and not larger than 256 MiB. |
| `channel_list` | A Nix channel package list | Stored text has no control or direction characters. |
| `catalog_id` | A package id | The id and source parsers do not panic. |
| `path_reference` | A local path | Encode and decode give the same bytes. |
| `tap_archive` | A tap tar archive | Extraction stays in its folder and makes only files and folders. |
| `tap_export` | A tap export | The conversion writes only in its output folder. |

A first byte of 0 makes `release_archive` and `tap_archive` compress the rest
as a raw tar. It makes `channel_list` wrap the rest as a Brotli stream. The
fuzzer then reaches the tar and JSON parsers without a valid compressed stream.

## Run

Install the nightly toolchain and `cargo-fuzz` first. The targets need Linux
or macOS.

```sh
cd fuzz
cargo +nightly fuzz run release_sums corpus/release_sums seeds/release_sums -- -max_total_time=300
```

On Linux, add `--target x86_64-unknown-linux-gnu` when `cargo-fuzz` selects
the musl target. `cargo fuzz` sets `cfg(fuzzing)`, which adds the entry points
in `pkg-cli` and `cask-catalog`. A normal build does not contain them.

`seeds/` holds a small valid input for each target. `corpus/` and `artifacts/`
are local and ignored.

CI compiles the targets on each pull request. `.github/workflows/fuzz.yml`
runs each target for 20 minutes every night. It keeps each corpus in the
Actions cache, and it uploads the input of a crash. A pull request that
changes `fuzz/` runs each target for 1 minute.
