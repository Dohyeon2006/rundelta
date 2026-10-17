"""Strict, audit-only warning taxonomy for the frozen five-case workload.
The product has string warnings, not typed diagnostics. Recognize only two stable
category prefixes, then require exact event semantics and original source evidence.
Unknown categories, extra occurrences and missing expected warnings fail closed.
This does not change product reliability, parser behavior or ranking.
"""
from collections import Counter
import re

CWD_PREFIX = re.compile(r'^raw/trace\.log:([1-9][0-9]*): path context unresolved;')
SHARED_PREFIX = 'shared filesystem state or root/namespace changes observed;'
FIELDS = ('type', 'kind', 'operation', 'operand', 'outcome', 'actor', 'mode')
TREE = '.git/objects/4b/825dc642cb6eb9a060e54bf8d69288fbee4904'
COMMIT = '.git/objects/79/951067511f83f81e9343de2c079f48c7b5b289'


def access(operand, outcome, mode, actor='root'):
    return ('cwd.relative_access', 'file', 'access', operand, outcome, actor, mode)


def expected_warnings(name, phase):
    """Explicit multiplicities for this fixture/toolchain, never learned from a run."""
    assert phase in ('good', 'repeat', 'bad'), phase
    if name == 'pkg-config':
        return Counter({access('~/.config/rpm/rpmrc', 'ENOENT', 'R_OK', 'rpm_eval_child'): 1})
    if name == 'git-identity':
        expected = Counter({access('.git/config', 'success', 'R_OK'): 2,
                            access(TREE, 'ENOENT', 'F_OK'): 1,
                            access('.git/hooks/post-index-change', 'ENOENT', 'X_OK'): 1})
        if phase != 'bad':
            expected[access('.git/config', 'success', 'R_OK', 'maintenance_child')] = 2
            expected[access(COMMIT, 'ENOENT', 'F_OK')] = 1
            for hook in ('pre-commit', 'prepare-commit-msg', 'commit-msg', 'post-commit'):
                expected[access('.git/hooks/'+hook, 'ENOENT', 'X_OK')] = 1
            expected[access('.git/hooks/reference-transaction', 'ENOENT', 'X_OK')] = 7
        return expected
    if name == 'cargo-linker':
        return Counter({('cwd.shared_clone_fs', 'spawn', 'clone/clone3',
                         'CLONE_FS', 'success', 'root_tree', 'global'): 1})
    return Counter()


def describe(counter):
    return [dict(zip(FIELDS, key), count=count) for key, count in sorted(counter.items())]


def source(event, raw_lines):
    evidence = event['evidence']
    assert evidence['file'] == 'raw/trace.log', evidence
    first, last = evidence['line'], evidence['end_line']
    assert isinstance(first, int) and isinstance(last, int) and 1 <= first <= last <= len(raw_lines), evidence
    assert re.fullmatch(r'[0-9]+', event['pid']), event['pid']
    line = raw_lines[first-1]
    assert re.match(r'^(?:'+event['pid']+r'\s+|\[pid\s+'+event['pid']+r'\]\s+)', line), evidence
    return line


def audit_warnings(name, phase, run, raw_lines):
    expected = expected_warnings(name, phase)
    metadata, events = run['metadata'], run['events']
    warnings = metadata['warnings']
    context = dict(case=name, phase=phase, expected=describe(expected), actual_warnings=warnings)
    # Expected uncertainty never upgrades a run to complete or bypasses storage checks.
    assert run['storage_integrity']['status'] == 'verified', context
    assert metadata['state'] == 'completed', context
    assert metadata['completeness'] == ('partial' if expected else 'complete_for_supported_events'), context
    bad_codes = {'python-venv':1, 'pkg-config':1, 'git-identity':128, 'gcc-header':1, 'cargo-linker':101}
    code = bad_codes[name] if phase == 'bad' else 0
    assert metadata['exit_code'] == metadata['collector_exit_code'] == code, context
    assert metadata['signal'] is None and metadata['collector_signal'] is None, context
    assert metadata['target_exit_raw'] == f'+++ exited with {code} +++', context
    root = next((e['pid'] for e in events if e['kind']=='exec' and e['outcome']=='success'), None)
    assert root is not None, context
    parents = {e['child_pid']:e['pid'] for e in events if e['kind']=='spawn' and e['outcome']=='success' and e['child_pid']}
    def actor(event):
        pid = event['pid']
        if pid == root:
            return 'root'
        if name == 'pkg-config' and parents.get(pid)==root and any(
            e['pid']==pid and e['kind']=='exec' and e['outcome']=='success'
            and e['argv']==['rpm','--eval','%{_target_cpu}-%{_vendor}-%{_target_os}%{?_gnu}']
            and e['path'] and e['path'].endswith('/rpm') for e in events
        ):
            return 'rpm_eval_child'
        if name == 'git-identity' and parents.get(pid)==root and any(
            e['pid']==pid and e['kind']=='exec' and e['outcome']=='success' and e['argv']
            and e['argv'][1:]==['maintenance','run','--auto','--no-quiet','--detach'] for e in events
        ):
            return 'maintenance_child'
        return 'other'
    def in_root_tree(pid):
        visited = set()
        while pid != root:
            if pid in visited or pid not in parents:
                return False
            visited.add(pid);pid=parents[pid]
        return True
    actual = Counter()
    covered = set()
    evidence_rows = []
    for warning in warnings:
        match = CWD_PREFIX.match(warning)
        if match:
            line = int(match[1])
            matches = [(i,e) for i,e in enumerate(events) if e['evidence']['file']=='raw/trace.log' and e['evidence']['line']==line]
            assert len(matches)==1, context
            i,event = matches[0]
            assert i not in covered, context
            assert event['context_uncertain'] is True and event['kind']=='file' and event['operation']=='access', context
            assert event['cwd_before']==metadata['cwd'], context
            assert event['evidence']['end_line']==line, context
            path = event['path']
            assert path and path.startswith('relative:') and path.endswith(' [cwd unresolved]'), context
            operand = path[len('relative:'):-len(' [cwd unresolved]')]
            raw = source(event, raw_lines)
            # Only this audit's simple access call is supported; no general trace parser.
            call = re.search(r'^(?:'+event['pid']+r'\s+|\[pid\s+'+event['pid']+r'\]\s+)access\("((?:\\x[0-9a-fA-F]{2}|[^"\\])*)", (R_OK|F_OK|X_OK)\)\s*=\s*(0|-1 ([A-Z0-9_]+))(?:\s+\([^)]*\))?\s*$', raw)
            assert call is not None, context
            raw_operand = re.sub(r'\\x([0-9a-fA-F]{2})', lambda m:chr(int(m[1],16)), call[1])
            assert raw_operand == operand, context
            assert event['outcome'] == ('success' if call[3]=='0' else call[4]), context
            key = access(operand, event['outcome'], call[2], actor(event))
            actual[key] += 1
            covered.add(i)
            evidence_rows.append(dict(type=key[0], actor=key[5], event_index=i, evidence=event['evidence']))
        elif warning.startswith(SHARED_PREFIX):
            assert name == 'cargo-linker', context
            triggers = [(i,e) for i,e in enumerate(events) if e['context_uncertain']]
            assert triggers, context
            assert all(e['cwd_before'] is None for e in events), context
            for i,event in triggers:
                assert i not in covered, context
                assert event['kind']=='spawn' and event['operation'] in ('clone','clone3'), context
                assert event['outcome']=='success' and event['child_pid'] and in_root_tree(event['pid']), context
                raw = source(event, raw_lines)
                assert re.search(r'\b'+event['operation']+r'\(', raw) and re.search(r'\bCLONE_FS\b',raw), context
                covered.add(i)
                evidence_rows.append(dict(type='cwd.shared_clone_fs', event_index=i, evidence=event['evidence']))
            actual[('cwd.shared_clone_fs','spawn','clone/clone3','CLONE_FS','success','root_tree','global')] += 1
        else:
            raise AssertionError(dict(context, unrecognized_warning=warning))
    # No extra uncertain event may be hidden by a missing diagnostic or a global warning.
    uncertain = {i for i,e in enumerate(events) if e['context_uncertain'] or e['kind']=='context' or (e['path'] or '').startswith('relative:')}
    assert covered == uncertain, dict(context, covered=sorted(covered), uncertain=sorted(uncertain))
    assert actual == expected, dict(context, actual=describe(actual))
    return dict(expected=describe(expected), actual=describe(actual), warnings=warnings,
                evidence=evidence_rows, capture=metadata['completeness'], storage=run['storage_integrity']['status'])
