# Preregistration: meson

URL: https://github.com/mesonbuild/meson/issues/16136
Title: cargo:  Meson is not able to automatically handle path dependencies in workspaces
Checked: 2026-09-20; state: open. No linked PR returned by open -linked:pr search; unlinked fixes not exhaustively excluded. Comments not read.

Body conditions: Attached workspace, Cargo build versus Meson setup, Python 3.14.7

Expected clue: Workspace Cargo.toml/path dependency reads or missing bar subproject paths

Success criterion: Cargo succeeds and Meson fails on workspace path dependency, not unrelated missing tools; dependency-resolution clue in first five.

Limits: traces do not prove file contents, causality, or upstream defect. Different tool commands in Meson/Cargo control are confounded and cannot isolate a single cause. No ranking changes. Original OS/version differences will be reported.
