# Preregistration: pybind

URL: https://github.com/pybind/pybind11/issues/3445
Title: [BUG]: pip installed pybind11 not picked up by CMake
Checked: 2026-09-20; state: open. No linked PR returned by open -linked:pr search; unlinked fixes not exhaustively excluded. Comments not read.

Body conditions: CMAKE_PREFIX_PATH points to installed package, then unset

Expected clue: Installed pybind11Config.cmake access disappears or package search failures; selected prefix environment changes

Success criterion: Same configure command succeeds with explicit package prefix and fails without it; relevant package discovery clue among first five.

Limits: traces do not prove file contents, causality, or upstream defect. Different tool commands in Meson/Cargo control are confounded and cannot isolate a single cause. No ranking changes. Original OS/version differences will be reported.
