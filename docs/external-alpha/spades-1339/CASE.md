# Preregistration: spades

URL: https://github.com/ablab/spades/issues/1339
Title: Linux build does not run in a directory containing spaces
Checked: 2026-09-20; state: open. No linked PR returned by open -linked:pr search; unlinked fixes not exhaustively excluded. Comments not read.

Body conditions: SPAdes 4.0.0 --test --careful, installation without/with spaces

Expected clue: Execution arguments or successful/missing dependency paths split at space; executable location change alone is insufficient diagnosis

Success criterion: Space-free installation succeeds; identical release in path with spaces produces the stated filesystem conversion error; malformed path/argument clue in first five.

Limits: traces do not prove file contents, causality, or upstream defect. Different tool commands in Meson/Cargo control are confounded and cannot isolate a single cause. No ranking changes. Original OS/version differences will be reported.
