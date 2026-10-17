#!/usr/bin/env python3
"""Verify the frozen public evidence and recomputed totals, without private traces."""
import hashlib
import json
import re
from pathlib import Path
import sys
sys.dont_write_bytecode = True
from summarize import load, summarize

HERE = Path(__file__).resolve().parent
manifest = json.loads((HERE/'freeze-manifest.json').read_text())
for name, digest in manifest['sha256'].items():
    assert hashlib.sha256((HERE/name).read_bytes()).hexdigest() == digest, name
actual = summarize(load())
stored = json.loads((HERE/'noise-summary.json').read_text())
# JSON object keys are strings, including the frequency-of-frequency keys.
assert json.loads(json.dumps(actual)) == stored
assert (actual['changes'],actual['file_calls'],actual['unique_paths']) == (446,712,420)
assert sum(g['changes'] for g in actual['groups']) == 446
assert len(actual['groups']) == 10
assert actual['parent_instances'] == 92
facts = json.loads((HERE/'evidence.json').read_text())
assert facts['original_issue_reproduced'] is False
assert facts['isolated_key_evidence']['rank'] == 177
assert facts['normal_repeat']['changes'] == 446
for name in manifest['sha256']:
    content=(HERE/name).read_text()
    # Disallow unredacted private home roots; generated placeholders are intentional.
    assert not re.search(r'/(?:home|Users)/', content), name
    assert ('rundelta-' + 'pybind5626-') not in content, name
print('PASS: frozen files; 446 keys / 712 calls / 420 paths; 10 categories; 92 parent instances; redaction checks')
