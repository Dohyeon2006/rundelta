#!/usr/bin/env python3
"""Batch-2 inventory, redaction, preregistration and old-scope verification.

The original 26/32/40-file validators remain byte-for-byte unchanged. Their
proper scope is verified in a temporary snapshot, not broadened to accept us.
"""
import ast
import hashlib
import html
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.parse

HERE = Path(__file__).resolve().parent
ALPHA = HERE.parent
CASES = ('pip-12996', 'bracket-lib-376')
COMMON = {'README.md', 'CHECKS.md', 'SCREENING.md', 'provenance.json', 'original-materials.json',
          'reproduce.py', 'analyze.py', 'verify.py', 'test_redaction.py'}
ALLOWED = COMMON | {name + '/' + file for name in CASES for file in ('CASE.md', 'RESULTS.md', 'summary.json', 'evidence.json')}
PRIVATE = re.compile(r'(?<![A-Za-z0-9_}])/(?:home|tmp|Users|root|var/tmp|var/folders|run/user|private/(?:tmp|var/folders))/|[A-Za-z]:[\\/]Users[\\/]', re.I)
TOKEN = re.compile(r'gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]+|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for v in value:
            yield from strings(v)
    elif isinstance(value, dict):
        for key, val in value.items():
            yield key
            yield from strings(val)


def decode_once(text):
    def char(match):
        value = match[1]
        number = int(value[1:], 16) if value[0] in ('x', 'u', 'U') else int(value, 8)
        try:
            return chr(number)
        except ValueError:
            raise AssertionError('invalid encoded character') from None
    # Unwrap one quoting layer before decoding C escapes. Decoding the second
    # slash of \\xHH too early would leave stray slashes between path letters.
    text = text.replace('\\/', '/').replace('\\\\', '\\')
    text = re.sub(r'(?<!\\)\\(x[0-9a-fA-F]{2}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|[0-7]{1,3})', char, text)
    return html.unescape(urllib.parse.unquote(text))


def check_text(text):
    """Reject plaintext plus nested JSON/C/strace, URL and bare-hex encodings."""
    for _ in range(12):
        assert not PRIVATE.search(text), 'private absolute path'
        assert not TOKEN.search(text), 'credential-shaped text'
        for match in re.finditer(r'(?<![0-9A-Za-z])(?:[0-9a-fA-F]{2}){4,}(?![0-9A-Za-z])', text):
            decoded = bytes.fromhex(match[0]).decode('utf-8', errors='ignore')
            assert not PRIVATE.search(decoded) and not TOKEN.search(decoded), 'hex-encoded private text'
        changed = decode_once(text)
        if changed == text:
            return
        text = changed
    raise AssertionError('excessive escape nesting; cannot verify')


def check_evidence(value):
    assert value['file'] == 'raw/trace.log'
    assert type(value['line']) is int and type(value['end_line']) is int
    assert 0 < value['line'] <= value['end_line']


def verify():
    actual = {str(p.relative_to(HERE)) for p in HERE.rglob('*') if p.is_file()}
    assert actual == ALLOWED, ('unexpected/missing batch files', actual - ALLOWED, ALLOWED - actual)
    for rel in sorted(actual):
        p = HERE / rel
        assert not p.is_symlink() and p.stat().st_size < 90_000, rel
        text = p.read_text()
        try:
            check_text(text)
            if p.suffix == '.json':
                for value in strings(json.loads(text)):
                    check_text(value)
            if p.suffix == '.py':
                parsed = ast.parse(text)
                for node in ast.walk(parsed):
                    if isinstance(node, ast.Constant) and isinstance(node.value, str):
                        check_text(node.value)
        except (AssertionError, ValueError) as error:
            raise AssertionError((rel, str(error))) from error
    before = json.loads((HERE / 'original-materials.json').read_text())
    assert len(before) == 40
    expected_alpha = set(before) | {'batch-2/' + p for p in ALLOWED}
    actual_alpha = {str(p.relative_to(ALPHA)) for p in ALPHA.rglob('*') if p.is_file()}
    assert actual_alpha == expected_alpha
    for name, sha in before.items():
        assert digest(ALPHA / name) == sha, ('old material changed', name)
    with tempfile.TemporaryDirectory(prefix='rundelta-alpha-40-verification-') as folder:
        snapshot = Path(folder)
        for name in before:
            dest = snapshot / name; dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ALPHA / name, dest)
        old = subprocess.run([sys.executable, '-B', str(snapshot / 'clone-fs-attribution/verify.py')],
                             capture_output=True, text=True)
        assert old.returncode == 0, old.stderr
        print('PASS: unchanged 40-file external-alpha scope; existing extension also verified its original 26/32-file scopes')
    live = subprocess.run([sys.executable, '-B', str(ALPHA / 'clone-fs-attribution/verify.py')], capture_output=True, text=True)
    assert live.returncode == 1 and 'assert actual == allowed' in live.stderr and 'batch-2/' in live.stderr
    print('EXPECTED REJECTION: existing fixed-inventory extension exits 1 on the expanded live tree; not counted as support for batch-2')
    provenance = json.loads((HERE / 'provenance.json').read_text())
    replay = provenance['public_script_replay']
    assert replay['collection_exit_code'] == replay['analysis_exit_code'] == 0 and replay['records'] == 8
    assert set(replay['cases']) == set(CASES)
    for name in CASES:
        case = HERE / name
        summary = json.loads((case / 'summary.json').read_text())
        evidence = json.loads((case / 'evidence.json').read_text())
        assert digest(case / 'CASE.md') == provenance['case_sha256'][name] == summary['preregistration_sha256']
        assert summary['case'] == name and summary['schema_version'] == 1
        repeated = replay['cases'][name]
        assert repeated['events_by_phase'] == {p: r['events'] for p, r in summary['phases'].items()}
        assert repeated['changes_by_comparison'] == {p: r['changes'] for p, r in summary['comparisons'].items()}
        assert repeated['diff_exits'] == {p: r['exit_code'] for p, r in summary['comparisons'].items()}
        assert repeated['clue_ranks'] == summary['clue_ranks']
        assert all(re.fullmatch('[0-9a-f]{64}', h) for h in repeated['full_json_sha256'].values())
        assert summary['source_tools_unchanged'] and summary['fixtures_unchanged'] and summary['control_valid']
        assert summary['selected_startup_environment'] == []
        assert [c['key'] for c in summary['changed_environment']] == ['PIP_CONFIG_FILE' if name == 'pip-12996' else 'PATH']
        assert set(summary['phases']) == {'good', 'repeat', 'bad', 'bad-repeat'}
        for phase, run in summary['phases'].items():
            assert run['state'] == 'completed' and run['storage'] == 'verified'
            assert run['target_exit'] == run['collector_exit'] == int(phase in ('bad', 'bad-repeat'))
            assert run['target_signal'] is None and run['collector_signal'] is None
            assert run['capture'] == ('partial' if run['warnings'] else 'complete_for_supported_events')
            if name == 'pip-12996':
                assert run['warnings'] == []
            else:
                warning, = run['warnings']
                assert warning['category'] == 'cwd.path_unresolved' and warning['access_mode'] == 'X_OK'
                event = warning['event']
                assert (event['kind'], event['operation'], event['path'], event['outcome'], event['context_uncertain']) == (
                    'file', 'access', 'relative:cmake [cwd unresolved]', 'ENOENT', True)
                check_evidence(event['evidence'])
        for label, report in summary['comparisons'].items():
            assert report['observed_differences'] is bool(report['changes'])
            assert sum(report['categories'].values()) == report['changes']
            assert len(report['top5']) == min(5, report['changes'])
            incomplete = any(side['capture'] != 'complete' for side in report['reliability'].values())
            assert all(side['storage'] == 'verified' for side in report['reliability'].values())
            assert report['exit_code'] == (3 if incomplete else int(bool(report['changes'])))
            assert incomplete == (name == 'bracket-lib-376')
            for change in report['top5']:
                for ref in change['left_evidence'] + change['right_evidence']:
                    check_evidence(ref)
        assert summary['clue_ranks'] == [{k: c[k] for k in ('name', 'rank', 'required')} for c in evidence['key_clues']]
        assert summary['strict_top5_hit'] is any(c['required'] and c['rank'] is not None and c['rank'] <= 5 for c in evidence['key_clues'])
        for clue in evidence['key_clues']:
            for observation in clue['observations']:
                assert observation['raw_semantics_verified']
                check_evidence(observation['event']['evidence'])
            if clue['rank'] is not None and clue['rank'] <= 5:
                change = summary['comparisons']['comparison']['top5'][clue['rank'] - 1]
                for observation in clue['observations']:
                    event = observation['event']
                    assert change['subject'] == event['path'] + ' (' + event['operation'] + ')'
                    assert event['outcome'] in change[observation['side']]
        print('PASS:', name, 'frozen preregistration, states, exact warning summary, ranks and local evidence coordinates')
    print('PASS: 17-file batch-2 inventory, plaintext/JSON/C/hex/octal/Unicode/URL redaction checks; full raw traces not included')


if __name__ == '__main__':
    verify()
