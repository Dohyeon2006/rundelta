# RunDelta contract baseline fixtures

These fixtures freeze the externally observable CLI and storage contracts used
by `tests/contract_baseline.rs`. They contain only synthetic paths, commands,
timestamps, IDs, and raw evidence.

- `storage/runs/abc001-101` and `abc002-102` are finalized metadata v3,
  events v2, marker v1 records.
- `storage/runs/abc101-201` is a markerless metadata/events v1 record.
- `storage/runs/abc102-202` is a markerless metadata/events v2 record.
- `golden/` contains exact stdout bytes. The test performs no broad
  normalization: all volatile fields are fixed by the fixtures, and any leaked
  temporary storage path is an explicit failure.
- `baseline_manifest.json` records SHA-256 facts for the review baseline. The
  hashes describe the named commit, not the current working tree.

The manifest hashes were calculated from Git object bytes with:

```text
git show 966fe1432d3053e8e66f24d423cc8c338ca7e543:<path> | sha256sum
```
