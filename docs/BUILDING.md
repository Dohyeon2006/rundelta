# Building and checking the public source

This Linux alpha requires Rust/Cargo, a compatible external strace and permission
to trace child processes. The observed toolchain is Rust/Cargo 1.98.1 on x86_64
Linux with strace 6.16. A minimum supported Rust version and a cross-distribution
support matrix have not been established. See the README for syscall coverage,
data handling and exit status semantics.

```bash
cargo build --release --locked
cargo test --locked
cargo test --locked --test public_cli -- --ignored
```

The last command additionally requires Python 3 and `cc`. Set `RUNDELTA_STRACE`
to a trusted strace executable when it is not in PATH. The public workflow uses
one command and one missing input file to exercise real captures, repeatability,
text/JSON output, diff statuses 0/1/2/3, a closed output pipe, and the guarantee
that a missing tracer does not execute the target. Reliability fixtures are
copies of newly recorded runs, not historical recordings. Test data is temporary
and is never part of the source distribution.

The eight-line parser fixture is hand-written test input, not a captured run.
The core unit tests are included unchanged. This public suite is intentionally
self-contained; it does not claim coverage of every platform or process model.

## Source and archive inspection

In this Git checkout, Python 3.11 or newer can run the independent safety checker
and its negative tests. It reports only relative names, rule names and counts.
It does not print matched data. The checker is a development tool and is not
included in the crate archive.

```bash
python3 -B tests/test_export_safety.py
python3 -B tools/check_export.py --root . --history
cargo package --list --locked
cargo package --locked
python3 -B tools/check_export.py --root . --history \
  --archive target/package/RunDelta-0.1.0.crate
```

For inspection of a clean export, keep build output outside its tree using
`CARGO_TARGET_DIR`. The source-tree check rejects extra files, including ignored
ones, so run it in a clean tree or pass `--archive` to inspect an archive
separately. The file list is explicit in Cargo.toml; Git ignore settings do not
decide what ships. Package builds and core unit tests do not require saved runs.

Scans cover plain text, JSON/JSONL strings, repeated string escapes, hexadecimal
and common standard/URL-safe base64 encodings. They cannot prove that arbitrary
encodings or unknown secret formats are absent. Five exact public source
constants involving the standard temporary root are reviewed exceptions scoped
to two Rust files; neither arbitrary temporary roots nor real records are
excepted. New files and new exceptions require review.

## Build paths in binaries

Default Rust release builds can retain compiler source locations in read-only
data, even without debug sections. Dependency and standard-library locations may
identify a builder's source cache. Stripping symbols does not necessarily remove
these paths. Source-package safety is not binary-package safety.

For a local release candidate, an external build environment can map the actual
source, dependency-cache and Rust sysroot prefixes to distinct stable logical
prefixes using rustc's `--remap-path-prefix`. Rebuild dependencies in a fresh
target directory and inspect the final binary. Do not delete error messages,
source line/column information or run-time user paths to conceal build paths.
Mapping compiler locations does not rewrite arbitrary source constants or the
contents of embedded resources. Public logical source paths may remain; these
are not private build roots.

No machine-specific flags or paths are committed here. No prebuilt binary is
part of this repository. Reproducing identical bytes across independent build
roots, testing other toolchains, and arranging debugger source-prefix mappings
remain separate work. Any binary distribution requires its own path scan and
dependency license review.

During export validation on 2026-09-20, a separate release build with external
prefix mappings retained 52 logical source locations: 32 under `/build/cargo`
and 20 under `/build/rust`. The scan found zero occurrences of the builder's
known private roots or private source paths in that binary. This is a measured
result for that toolchain and build, not a guarantee for default builds or a
proof of reproducible binaries. The binary and machine-specific invocation are
not part of this source tree.
