#!/usr/bin/env python3
"""Independent semantic audit of new five-case recordings; never rewrites records.

Only local evidence coordinates are used. This deliberately small raw decoder
accepts the traced syntax used by this workload, not approximate syscall matches.
Unsupported audited syntax fails closed. Historical audits are not imported.
"""
import argparse
from collections import Counter, defaultdict
import json
import os
from pathlib import Path
import re
import subprocess

from fresh_field_contract import BAD_CODES, CASES, access, audit_command, clue_for, describe, warnings_for

ROOT = Path(__file__).resolve().parents[1]
CWD_WARNING = re.compile(r'^raw/trace\.log:([1-9][0-9]*): path context unresolved;')
PREFIX = re.compile(r'^(?:([0-9]+)\s+|\[pid\s+([0-9]+)\]\s+)(.*)$')
RPM_ARGV = ['rpm', '--eval', '%{_target_cpu}-%{_vendor}-%{_target_os}%{?_gnu}']
GIT_ARGV = ['maintenance', 'run', '--auto', '--no-quiet', '--detach']
# Independent audit scope, deliberately not imported from the product parser.
# A selector extension needs an explicit oracle decision rather than inheriting
# whatever events a newer parser happens to emit.
RAW_KINDS = {
    **dict.fromkeys(('execve', 'execveat'), 'exec'),
    **dict.fromkeys(('open', 'openat', 'openat2', 'access', 'faccessat', 'faccessat2'), 'file'),
    **dict.fromkeys(('chdir', 'fchdir'), 'cwd'),
    **dict.fromkeys(('clone', 'clone3', 'fork', 'vfork'), 'spawn'),
    **dict.fromkeys(('unshare', 'setns', 'chroot', 'pivot_root', 'mount', 'umount2',
                    'move_mount', 'mount_setattr'), 'context'),
}


def location(evidence):
    return evidence['file'], evidence['line'], evidence['end_line']


def prefixed(line):
    m = PREFIX.fullmatch(line)
    assert m, ('missing task identity', line)
    return m[1] or m[2], m[3]


def source(event, raw):
    """Join only a proven same-PID, same-syscall unfinished/resumed pair."""
    file, first, last = location(event['evidence'])
    assert file == 'raw/trace.log' and type(first) is int and type(last) is int
    assert 1 <= first <= last <= len(raw), event['evidence']
    pid, body = prefixed(raw[first - 1])
    assert pid == event['pid'], ('wrong evidence PID', event['evidence'])
    if first == last:
        assert '<unfinished ...>' not in body and not body.startswith('<... ')
        return body
    start = re.fullmatch(r'([a-z0-9_]+)\(.*<unfinished \.\.\.>', body)
    assert start, ('missing unfinished entry', event['evidence'])
    end_pid, tail = prefixed(raw[last - 1])
    resumed = re.fullmatch(r'<\.\.\. ([a-z0-9_]+) resumed>(.*)', tail)
    assert end_pid == pid and resumed and resumed[1] == start[1], ('mismatched resume', event['evidence'])
    # A task cannot start another syscall while its original call is pending.
    # Signals and other tasks may interleave; they are not part of this event.
    for line in raw[first:last - 1]:
        middle_pid, middle = prefixed(line)
        assert middle_pid != pid or middle.startswith('--- '), ('intervening call from same task', line)
    return body[:-len('<unfinished ...>')].rstrip() + resumed[2]


def split_args(text):
    """Split strace arguments, respecting strings, containers and fd annotations."""
    parts, stack, quote, escape, start = [], [], False, False, 0
    for i, ch in enumerate(text):
        if quote:
            if escape:
                escape = False
            elif ch == '\\':
                escape = True
            elif ch == '"':
                quote = False
        elif ch == '"':
            quote = True
        elif ch in '([{':
            stack.append({'(': ')', '[': ']', '{': '}'}[ch])
        elif ch == '<' and text[i + 1:i + 3] == '\\x':
            stack.append('>')
        elif stack and ch == stack[-1]:
            stack.pop()
        elif ch == ',' and not stack:
            parts.append(text[start:i].strip()); start = i + 1
    assert not quote and not stack, ('unbalanced arguments', text)
    if text.strip():
        parts.append(text[start:].strip())
    return parts


def string(text):
    assert re.fullmatch(r'"(?:\\x[0-9a-fA-F]{2}|[^"\\])*"', text), ('unsupported string', text)
    data = re.sub(r'\\x([0-9a-fA-F]{2})', lambda m: chr(int(m[1], 16)), text[1:-1])
    # RunDelta's reversible byte spelling; no filesystem or Unicode normalization.
    return ''.join(c if 32 <= ord(c) <= 126 and c != '\\' else '\\x%02x' % ord(c) for c in data)


def fd_path(token):
    m = re.search(r'<((?:\\x[0-9a-fA-F]{2})+)>$', token)
    if not m:
        return None
    path = string('"' + m[1] + '"')
    return path if path.startswith('/') else None


def syscall(event, raw):
    body = source(event, raw)
    # Descriptor attributes (e.g. /dev/null<char 1:3>) are opaque here. They
    # prove a numeric return, never a pathname; fd_path requires fully -xx bytes.
    m = re.fullmatch(r'([a-z0-9_]+)\((.*)\)\s+=\s+(-?[0-9]+(?:<[^\n]*>)?)(?:\s+(.*))?', body)
    assert m, ('unsupported audited syscall syntax', event['evidence'], body)
    op, args, value, extra = m[1], split_args(m[2]), m[3], m[4]
    number = int(value.split('<')[0])
    if number == -1:
        error = re.fullmatch(r'([A-Z][A-Z0-9_]+)(?: \([^\n]*\))?', extra or '')
        assert error, ('unsupported return', body)
        outcome = error[1]
    else:
        assert number >= 0 and extra is None, ('unsupported successful return', body)
        outcome = 'success'
    assert event['operation'] == op and event['outcome'] == outcome, ('syscall/result mismatch', event)
    return op, args, value


class Evidence:
    def __init__(self, run, raw):
        self.run, self.raw, self.events = run, raw, run['events']
        self.by_location = {}
        self.by_pid = defaultdict(list)
        self.parents = {}
        self.decoded = {}
        for event in self.events:
            key = location(event['evidence'])
            assert key not in self.by_location, ('ambiguous evidence', key)
            self.by_location[key] = event
            self.by_pid[event['pid']].append(event)
            if event['kind'] == 'spawn' and event['outcome'] == 'success':
                child = event['child_pid']
                assert child and child not in self.parents, ('ambiguous task ancestry', child)
                self.parents[child] = event
        roots = {e['pid'] for e in self.events if e['kind'] == 'exec' and e['outcome'] == 'success'
                 and e['pid'] not in self.parents}
        assert len(roots) == 1, ('ambiguous root', roots)
        self.root = next(iter(roots))
        for events in self.by_pid.values():
            events.sort(key=lambda e: location(e['evidence'])[1:])
        root_exec = next(e for e in self.by_pid[self.root] if e['kind'] == 'exec' and e['outcome'] == 'success')
        self.validate(root_exec)
        assert root_exec['argv'] == run['metadata']['command'], 'root argv differs from registered command'

    def validate(self, event):
        key = location(event['evidence'])
        if key in self.decoded:
            return self.decoded[key]
        if event['kind'] == 'exit':
            assert event['operation'] == 'exit'
            body = source(event, self.raw)
            assert event['outcome'] == body, ('exit/evidence mismatch', key)
            self.decoded[key] = ('exit', [], body)
            return self.decoded[key]
        op, args, value = syscall(event, self.raw)
        kind = event['kind']
        assert RAW_KINDS.get(op) == kind, ('syscall/event family mismatch', key, op, kind)
        if kind == 'spawn':
            assert op in ('clone', 'clone3', 'fork', 'vfork')
            assert event['child_pid'] == (value if event['outcome'] == 'success' else None)
        elif kind in ('exec', 'file'):
            assert op in (('execve', 'execveat') if kind == 'exec' else
                          ('open', 'openat', 'openat2', 'access', 'faccessat', 'faccessat2'))
            at = op in ('openat', 'openat2', 'faccessat', 'faccessat2', 'execveat')
            operand = string(args[int(at)])
            path = operand
            if not operand.startswith('/'):
                observed = fd_path(value) if op in ('open', 'openat', 'openat2') and event['outcome'] == 'success' else None
                base = fd_path(args[0]) if at else None
                path = observed or (base + ('' if base.endswith('/') else '/') + operand if base else 'relative:' + operand +
                                    (' [dirfd unresolved]' if at and not args[0].startswith('AT_FDCWD') else ' [cwd unresolved]'))
            assert event['path'] == path, ('path/evidence mismatch', event, path)
            assert event['context_uncertain'] is path.startswith('relative:'), ('wrong path certainty', event)
            if kind == 'exec':
                argv = args[2 if at else 1]
                assert argv.startswith('[') and argv.endswith(']'), 'missing exec arguments'
                assert event['argv'] == [string(a) for a in split_args(argv[1:-1])], 'exec argv/evidence mismatch'
        self.decoded[key] = (op, args, value)
        return op, args, value

    def exec_before(self, pid, event):
        candidates = [e for e in self.by_pid[pid] if e['kind'] == 'exec' and e['outcome'] == 'success'
                      and e['evidence']['end_line'] <= event['evidence']['line']]
        if not candidates:
            return None
        self.validate(candidates[-1])
        return candidates[-1]

    def parent(self, pid, before):
        event = self.parents.get(pid)
        assert event and event['evidence']['line'] < before, ('unproven task ancestry', pid)
        self.validate(event)
        return event

    def actor(self, event):
        pid = event['pid']
        if pid == self.root:
            return 'root'
        lineage, current = [], pid
        before = event['evidence']['line']
        while current != self.root:
            assert current not in lineage, 'cyclic ancestry'
            lineage.append(current)
            birth = self.parent(current, before)
            current, before = birth['pid'], birth['evidence']['line']
        direct = self.parents[pid]['pid'] == self.root
        own = self.exec_before(pid, event)
        if direct and own:
            if own['path'].rsplit('/', 1)[-1] == 'rpm' and own['argv'] == RPM_ARGV:
                return 'rpm_eval_child'
            if own['path'].rsplit('/', 1)[-1] == 'git' and own['argv'][1:] == GIT_ARGV:
                return 'maintenance_child'
            if own['path'].rsplit('/', 1)[-1] == 'cc1' and 'strace.c' in own['argv']:
                return 'compiler_child'
        # rustc may create a worker thread, which then spawns its linker. Only
        # explicit CLONE_THREAD edges with no intervening exec inherit that role.
        if own is None and event['kind'] == 'exec':
            birth = self.parents[pid]
            task, limit = birth['pid'], birth
            while task != self.root:
                exe = self.exec_before(task, limit)
                if exe:
                    if exe['path'].rsplit('/', 1)[-1] == 'rustc' and '--crate-name' in exe['argv']:
                        return 'rustc_linker_child'
                    break
                edge = self.parent(task, limit['evidence']['line'])
                op, args, _ = self.validate(edge)
                assert op in ('clone', 'clone3') and re.search(r'\bCLONE_THREAD\b', ','.join(args)), 'unproven rustc thread role'
                task, limit = edge['pid'], edge
        return 'other'


def audit_normalization_coverage(evidence):
    """Require a bijection between raw observations and normalized coordinates.

    This checks syntax, families, results and existing file/exec evidence rules;
    it does not replay production FS-context transitions or task generations.
    Signal delivery notifications are the sole accepted raw-only family. Any
    unsupported or incomplete syntax fails this fresh workload oracle with its
    coordinate and reason rather than becoming an implicit exclusion.
    """
    pending, observations, excluded = {}, {}, []
    for number, line in enumerate(evidence.raw, 1):
        pid, body = prefixed(line)
        first = number
        if body.endswith('<unfinished ...>'):
            start = re.fullmatch(r'([a-z0-9_]+)\(.*<unfinished \.\.\.>', body)
            assert start, ('unsupported unfinished syntax', number, body)
            assert start[1] in RAW_KINDS, ('unsupported raw operation', number, start[1])
            assert pid not in pending, ('replaced unfinished raw call', number, pid)
            pending[pid] = (first, start[1], body[:-len('<unfinished ...>')].rstrip())
            continue
        if body.startswith('<... '):
            resumed = re.fullmatch(r'<\.\.\. ([a-z0-9_]+) resumed>(.*)', body)
            assert resumed and pid in pending, ('unmatched resumed raw call', number, pid)
            first, operation, prefix = pending.pop(pid)
            assert resumed[1] == operation, ('mismatched resumed raw call', first, number)
            body = prefix + resumed[2]
        else:
            assert pid not in pending or body.startswith('--- '), ('intervening raw call from same task', number, pid)
        key = ('raw/trace.log', first, number)
        if body.startswith('--- '):
            assert re.fullmatch(r'--- [A-Z0-9_+]+(?: .*)? ---', body), ('unsupported signal notification', number, body)
            excluded.append(dict(line=number, reason='signal delivery is outside normalized Event-v2 families'))
            continue
        if body.startswith('+++ '):
            exited = re.fullmatch(r'\+\+\+ exited with ([0-9]+) \+\+\+', body)
            killed = re.fullmatch(r'\+\+\+ killed by ([A-Za-z0-9_+() -]+) \+\+\+', body)
            assert (exited and int(exited[1]) <= 255) or killed, ('unsupported process status', number, body)
            operation, kind = 'exit', 'exit'
        else:
            call = re.match(r'([a-z0-9_]+)\(', body)
            assert call and call[1] in RAW_KINDS, ('unsupported raw operation or syntax', number, body)
            operation, kind = call[1], RAW_KINDS[call[1]]
        observations[key] = (pid, operation, kind)
    assert not pending, ('unfinished raw calls at end of trace', sorted(pending))
    raw_keys, event_keys = set(observations), set(evidence.by_location)
    assert not raw_keys - event_keys, ('raw observation missing normalized event', sorted(raw_keys - event_keys))
    assert not event_keys - raw_keys, ('normalized event has no raw observation', sorted(event_keys - raw_keys))
    for key, (pid, operation, kind) in observations.items():
        event = evidence.by_location[key]
        assert (event['pid'], event['operation'], event['kind']) == (pid, operation, kind), ('raw/normalized identity mismatch', key)
        evidence.validate(event)
    metadata = evidence.run['metadata']
    # Older independent synthetic fixtures omit these fields; when present they
    # must agree with actual coverage, not merely with two coerced summaries.
    if 'parsed_events' in metadata:
        assert metadata['parsed_events'] == len(evidence.events), 'wrong parsed event count'
    if 'raw_lines' in metadata:
        assert metadata['raw_lines'] == len(evidence.raw), 'wrong raw line count'
    return dict(raw_observations=len(observations), normalized_events=len(evidence.events), excluded=excluded)


def audit_warnings(name, phase, evidence):
    audit_normalization_coverage(evidence)
    run, raw, events = evidence.run, evidence.raw, evidence.events
    metadata = run['metadata']
    expected = warnings_for(name, phase)
    assert run['storage_integrity']['status'] == 'verified'
    assert metadata['state'] == 'completed'
    assert metadata['completeness'] == ('partial' if expected else 'complete_for_supported_events')
    code = BAD_CODES[name] if phase == 'bad' else 0
    assert metadata['exit_code'] == metadata['collector_exit_code'] == code
    assert metadata['signal'] is None and metadata['collector_signal'] is None
    assert metadata['target_exit_raw'] == f'+++ exited with {code} +++'
    exits = [e for e in events if e['kind'] == 'exit' and e['pid'] == evidence.root]
    assert len(exits) == 1 and source(exits[0], raw) == exits[0]['outcome'] == metadata['target_exit_raw']
    actual, covered, rows = Counter(), set(), []
    for warning in metadata['warnings']:
        match = CWD_WARNING.match(warning)
        assert match, ('undeclared warning category', name, phase, warning)
        matches = [e for e in events if e['evidence']['file'] == 'raw/trace.log' and e['evidence']['line'] == int(match[1])]
        assert len(matches) == 1, ('ambiguous warning source', warning)
        event = matches[0]
        key = location(event['evidence'])
        assert key not in covered, ('duplicate warning', warning)
        op, args, _ = evidence.validate(event)
        assert event['kind'] == 'file' and op == 'access' and len(args) == 2
        assert event['context_uncertain'] is True and event['cwd_before'] == metadata['cwd']
        assert event['path'] == 'relative:' + string(args[0]) + ' [cwd unresolved]'
        semantic = access(string(args[0]), event['outcome'], args[1], evidence.actor(event))
        actual[semantic] += 1
        covered.add(key)
        rows.append(dict(type=semantic[0], actor=semantic[5], evidence=event['evidence']))
    uncertain = {location(e['evidence']) for e in events if e['context_uncertain'] or e['kind'] == 'context'
                 or (e['path'] or '').startswith('relative:')}
    assert covered == uncertain, ('uncovered uncertainty', sorted(uncertain - covered))
    assert actual == expected, dict(case=name, phase=phase, expected=describe(expected), actual=describe(actual))
    return dict(expected=describe(expected), actual=describe(actual), evidence=rows,
                warning_count=len(metadata['warnings']), split_warning_count=sum(e['evidence']['line'] != e['evidence']['end_line'] for e in rows))


def audit_clue(contract, report, left, right):
    subject = contract['path'] + ' (' + contract['operation'] + ')'
    matches = [(i + 1, c) for i, c in enumerate(report['changes']) if c['subject'] == subject]
    assert len(matches) == 1, ('missing/duplicate key clue', subject)
    rank, change = matches[0]
    assert rank <= contract['max_rank'], ('clue outside top five', rank)
    if contract['exact_rank'] is not None:
        assert rank == contract['exact_rank'], ('changed fixed ranking', rank)
    for field in ('category', 'left', 'right'):
        assert change[field] == contract[field], ('wrong clue semantics', field, change)
    for side, evidence in [('left', left), ('right', right)]:
        refs = change[side + '_evidence']
        assert bool(refs) == bool(contract[side]), ('missing/unexpected evidence', side)
        for ref in refs:
            event = evidence.by_location[location(ref)]
            evidence.validate(event)
            assert (event['kind'], event['operation'], event['path']) == (contract['kind'], contract['operation'], contract['path'])
            assert event['outcome'] in contract[side]
            assert contract['path_semantics'] == 'observed_absolute' and event['path'].startswith('/') and not event['context_uncertain']
            assert evidence.actor(event) == contract['actor'], ('wrong clue actor', evidence.actor(event), contract['actor'])
        actors = Counter()
        for event in evidence.events:
            if (event['kind'], event['operation'], event['path']) == (contract['kind'], contract['operation'], contract['path']):
                evidence.validate(event)
                assert event['outcome'] in contract[side]
                actors[evidence.actor(event)] += 1
        assert actors == contract['actors_' + side], ('wrong clue origins/counts', side, actors, contract['actors_' + side])
    return rank


def audit_report(data, text, rc, text_rc, left, right, left_warnings, right_warnings):
    # A caller may audit an already loaded report without invoking audit_warnings.
    # Recheck both raw ledgers so shared omissions cannot look like equality.
    for evidence in (left, right):
        audit_normalization_coverage(evidence)
    assert data['schema_version'] == 4 and data['errors'] == []
    assert data['observed_differences'] is bool(data['changes'])
    expected_rc = 3 if left_warnings['actual'] or right_warnings['actual'] else int(bool(data['changes']))
    assert rc == text_rc == data['exit_code'] == expected_rc
    assert ('RELIABILITY NOT ESTABLISHED' in text) == (expected_rc == 3)
    assert f'Observed differences: {"yes" if data["changes"] else "no"}\n' in text
    assert f'Exit code: {expected_rc}\n' in text
    assert 'Storage integrity: left=verified, right=verified\n' in text
    assert f'Capture completeness: left={data["reliability"]["left"]["capture"]}, right={data["reliability"]["right"]["capture"]}\n' in text
    for side, ev, warnings in [('left', left, left_warnings), ('right', right, right_warnings)]:
        assert data[side] == ev.run['metadata']['name'] and data[side + '_id'] == ev.run['metadata']['id']
        assert data['reliability'][side] == dict(capture='incomplete' if warnings['actual'] else 'complete', storage='verified')
    assert len(data['warnings']) == bool(left_warnings['actual']) + bool(right_warnings['actual'])
    for ev, warnings in [(left, left_warnings), (right, right_warnings)]:
        if warnings['actual']:
            expected = f"{ev.run['metadata']['name']}: state=completed, {warnings['warning_count']} parser warnings; inspect metadata.json"
            assert expected in data['warnings']
    assert f'Total observed changes: {len(data["changes"])}' in text
    groups = Counter(c['detail_group'] for i, c in enumerate(data['changes']) if i >= 5 and c['detail_group'])
    for group, count in groups.items():
        assert group in text and f'{count} changes folded' in text
    position = 0
    for change in data['changes'][:5]:
        block = '\n' + change['category'] + '\n'
        if change['subject']:
            block += '  ' + change['subject'] + '\n'
        block += '  ' + (', '.join(change['left']) or '(not observed)') + ' → ' + (', '.join(change['right']) or '(not observed)') + '\n'
        found = text.find(block, position)
        assert found >= 0, ('text top five differ from JSON', change)
        position = found + len(block)
    # Check every report reference, not just the key clue. Outcomes are compared
    # against all matching normalized events; raw file/exec evidence is checked too.
    aggregated = []
    for evidence in (left, right):
        rows = defaultdict(set)
        for event in evidence.events:
            if event['kind'] in ('file', 'exec'):
                evidence.validate(event)
                rows[(event['kind'], event['operation'], event['path'])].add(event['outcome'])
        aggregated.append(rows)
    seen = set()
    for c in data['changes']:
        if not c['subject']:
            assert c['category'] in ('Exit status', 'Capture completeness') and not c['left_evidence'] and not c['right_evidence']
            if c['category'] == 'Exit status':
                for side, ev in [('left', left), ('right', right)]:
                    assert c[side] == [f"code=Some({ev.run['metadata']['exit_code']}), signal=None"]
            else:
                for side, ev in [('left', left), ('right', right)]:
                    m = ev.run['metadata']
                    assert c[side] == [f"{m['state']}; {m['completeness']}; {len(m['warnings'])} warnings"]
            continue
        subject = re.fullmatch(r'(.*) \(([a-z0-9_]+)\)', c['subject'])
        assert subject, ('unsupported report subject', c)
        kind = 'exec' if c['category'] in ('Executable changes', 'Execution attempt changes') else 'file'
        assert c['category'] in ('Executable changes', 'Execution attempt changes', 'File access outcome changes', 'New file access', 'Removed file access')
        key = kind, subject[2], subject[1]
        assert key not in seen, ('duplicate change', key)
        seen.add(key)
        expected_category = ('Execution attempt changes' if any(o != 'success' for o in c['left'] + c['right']) else 'Executable changes') if kind == 'exec' else (
            'New file access' if not c['left'] else 'Removed file access' if not c['right'] else 'File access outcome changes')
        assert c['category'] == expected_category, ('wrong change category', c)
        for side, evidence, aggregate in [('left', left, aggregated[0]), ('right', right, aggregated[1])]:
            assert c[side] == sorted(aggregate.get(key, set()))
            assert bool(c[side + '_evidence']) == bool(c[side])
            for ref in c[side + '_evidence']:
                event = evidence.by_location[location(ref)]
                assert (event['kind'], event['operation'], event['path']) == key and event['outcome'] in c[side]
    changed_keys = {key for key in aggregated[0].keys() | aggregated[1].keys() if aggregated[0].get(key, set()) != aggregated[1].get(key, set())}
    assert seen == changed_keys, ('omitted/extra observed changes', changed_keys - seen, seen - changed_keys)
    return groups


def cli(binary, work, *args):
    p = subprocess.run([str(binary), '--storage', str(work / 'records'), *args], capture_output=True, text=True, timeout=120)
    return p.returncode, p.stdout


def audit(work, binary):
    results = json.loads((work / 'results.json').read_text())
    assert [c['case'] for c in results] == list(CASES), 'five registered cases, including Git, required'
    summary = []
    for name in CASES:
        runs, warning_rows = {}, {}
        for phase in ('good', 'repeat', 'bad'):
            rc, stdout = cli(binary, work, 'show', name + '-' + phase, '--json')
            assert rc == 0, (name, phase, rc)
            run = json.loads(stdout)
            metadata = run['metadata']
            assert re.fullmatch(r'[0-9a-f]+-[0-9]+', metadata['id']) and metadata['name'] == name + '-' + phase
            raw = (work / 'records/runs' / metadata['id'] / 'raw/trace.log').read_text().splitlines()
            assert metadata['parsed_events'] == len(run['events']) and metadata['raw_lines'] == len(raw)
            runs[phase] = Evidence(run, raw)
            warning_rows[phase] = audit_warnings(name, phase, runs[phase])
            print('WARNINGS ' + json.dumps(dict(case=name, phase=phase, **warning_rows[phase]), sort_keys=True))
        metadata = [runs[p].run['metadata'] for p in ('good', 'repeat', 'bad')]
        assert all(m['command'] == metadata[0]['command'] and m['cwd'] == metadata[0]['cwd'] for m in metadata)
        expected_cwd = ROOT if name == 'cargo-linker' else work / {'python-venv': 'python-venv', 'pkg-config': 'pkg-config', 'git-identity': 'git-identity/repo', 'gcc-header': 'gcc-header/strace-source/src'}[name]
        assert metadata[0]['cwd'] == str(expected_cwd)
        audit_command(name, metadata[0]['command'], work, ROOT)
        reports = {}
        for phase in ('repeat', 'bad'):
            args = ('diff', name + '-good', name + '-' + phase)
            rc, stdout = cli(binary, work, *args, '--json')
            text_rc, text = cli(binary, work, *args)
            data = json.loads(stdout)
            audit_report(data, text, rc, text_rc, runs['good'], runs[phase], warning_rows['good'], warning_rows[phase])
            reports[phase] = data
        assert not any(c['category'] in ('Executable changes', 'Execution attempt changes') for c in reports['repeat']['changes']), (name, 'normal-control execution drift')
        contract = clue_for(name, work)
        rank = audit_clue(contract, reports['bad'], runs['good'], runs['bad'])
        row = dict(case=name, rank=rank, max_rank=contract['max_rank'], exact_rank=contract['exact_rank'],
                   comparison_changes=len(reports['bad']['changes']), control_changes=len(reports['repeat']['changes']),
                   comparison_exit=reports['bad']['exit_code'], control_exit=reports['repeat']['exit_code'],
                   reliability=reports['bad']['reliability'], warning_audit=warning_rows)
        summary.append(row)
        print(f'PASS {name}: clue={rank}, control={row["control_changes"]}, diff={row["comparison_exit"]}')
    print('PASS: fresh five-case semantic audit; all local evidence, exact warnings, ranking and reliability checked')
    return summary


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('work', type=Path)
    parser.add_argument('--binary', type=Path, default=Path(os.environ.get('RUNDELTA_BIN', ROOT / 'target/debug/rundelta')))
    parser.add_argument('--output', type=Path, help='New summary file only; existing files are never overwritten')
    args = parser.parse_args()
    result = audit(args.work.resolve(), args.binary.resolve())
    if args.output:
        with args.output.open('x') as stream:
            json.dump(result, stream, indent=2); stream.write('\n')
