#!/usr/bin/env python3
"""Read private batch-2 observations; create bounded, redacted public summaries.

Does not alter records, rank observations, or change product reliability.
The output directory must not exist. Full JSON and raw traces stay private.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    return json.loads(path.read_text())


def decoded(text):
    return re.sub(r'(?:\\x[0-9a-fA-F]{2})+',
                  lambda m: bytes.fromhex(m[0].replace('\\x', '')).decode('utf-8', 'backslashreplace'), text)


def redact(value, replacements):
    if isinstance(value, str):
        value = decoded(value)
        for old, new in replacements:
            value = value.replace(old, new)
        return value
    if isinstance(value, list):
        return [redact(v, replacements) for v in value]
    if isinstance(value, dict):
        return {k: redact(v, replacements) for k, v in value.items()}
    return value


def reference(run, evidence):
    matches = [e for e in run['events'] if e['evidence'] == evidence]
    assert len(matches) == 1, ('ambiguous evidence', evidence)
    return matches[0]


def raw_call(case, run, event):
    """Only join this record's matching PID/syscall endpoints, never old lines."""
    ev = event['evidence']
    assert ev['file'] == 'raw/trace.log'
    raw = (case / 'records/runs' / run['metadata']['id'] / ev['file']).read_text().splitlines()
    first, last = ev['line'], ev['end_line']
    assert 1 <= first <= last <= len(raw)
    def body(line):
        m = re.fullmatch(r'(\d+)\s+(.*)', line)
        assert m and m[1] == event['pid'], 'evidence role mismatch'
        return m[2]
    start = body(raw[first - 1])
    assert start.startswith(event['operation'] + '(')
    if first != last:
        assert start.endswith('<unfinished ...>')
        end = re.fullmatch(r'<\.\.\. ([a-z0-9_]+) resumed>(.*)', body(raw[last - 1]))
        assert end and end[1] == event['operation']
        start = start[:-len('<unfinished ...>')].rstrip() + end[2]
    else:
        assert '<unfinished ...>' not in start
    return decoded(start)


def key_evidence(case, left, right, report, specs):
    rows = []
    for spec in specs:
        matches = [(i + 1, c) for i, c in enumerate(report['changes'])
                   if c['category'] == spec['category'] and c['subject'] == spec['path'] + ' (' + spec['operation'] + ')'
                   and c['left'] == spec['left'] and c['right'] == spec['right']]
        assert len(matches) <= 1
        row = dict(name=spec['name'], rank=matches[0][0] if matches else None, required=spec['required'], observations=[])
        if matches:
            _, change = matches[0]
            for side, run in [('left', left), ('right', right)]:
                root = next(e['pid'] for e in run['events'] if e['kind'] == 'exec' and e['outcome'] == 'success')
                for ref in change[side + '_evidence']:
                    event = reference(run, ref)
                    assert event['kind'] == spec['kind'] and event['operation'] == spec['operation']
                    assert event['path'] == spec['path'] and event['outcome'] == 'success' and not event['context_uncertain']
                    call = raw_call(case, run, event)
                    operand = re.search(r'"([^"\n]*)"', call)
                    assert operand and operand[1] == spec['path'], 'key path not proven by absolute raw operand'
                    assert re.search(r'\)\s+=\s+[0-9]+(?:<|\s|$)', call), 'key result is not successful in raw'
                    row['observations'].append(dict(side=side, role='root' if root == event['pid'] else 'descendant',
                                                    event={k: event[k] for k in ['kind', 'operation', 'path', 'outcome', 'context_uncertain', 'evidence']},
                                                    raw_semantics_verified=True))
            assert row['observations'], 'rank without evidence'
            assert any(e['role'] == 'root' for e in row['observations']), 'key clue needs a proven root observation'
        rows.append(row)
    return rows


def analyze(work, output):
    assert not output.exists(), 'never overwrite a previous analysis'
    original_files = {p: digest(p) for name in ('pip-12996', 'bracket-lib-376')
                      for p in (work / name).rglob('*') if p.is_file() and ('records' in p.parts or p.suffix in ('.json', '.stdout', '.stderr', '.txt'))}
    prereg = read(work / 'preregistered.json')
    collected = read(work / 'collection.json')
    assert collected['source_tools_unchanged'] and prereg['case_sha256'] == collected['case_sha256']
    output.mkdir()
    for name in ('pip-12996', 'bracket-lib-376'):
        case = work / name
        runs = {phase: read(case / (phase + '.show.json')) for phase in ('good', 'repeat', 'bad', 'bad-repeat')}
        conditions = read(case / 'conditions.json')
        good_env, bad_env = conditions['good_environment'], conditions['bad_environment']
        differing = sorted(k for k in good_env.keys() | bad_env.keys() if good_env.get(k) != bad_env.get(k))
        assert differing == [conditions['changed_key']]
        assert all(r['metadata']['command'] == conditions['command'] and r['metadata']['cwd'] == conditions['cwd'] for r in runs.values())
        assert all(r['metadata']['environment'] == {} for r in runs.values()), 'startup variables were not preselected'
        comparison = read(case / 'comparison.json')
        if name == 'pip-12996':
            specs = [dict(name='override_config_opened', path=str(case / 'override.conf'), category='New file access',
                          kind='file', operation='openat', left=[], right=['success'], required=True),
                     dict(name='user_config_no_longer_observed', path=str(case / 'home/.config/pip/pip.conf'), category='Removed file access',
                          kind='file', operation='openat', left=['success'], right=[], required=True)]
            matched = all(runs[p]['metadata']['exit_code'] == 0 and (case / (p + '.stdout')).read_text().strip() == 'constraints.txt' for p in ('good', 'repeat'))
            matched &= all(runs[p]['metadata']['exit_code'] == 1 and 'ERROR: No such key - install.constraint' in (case / (p + '.stderr')).read_text() for p in ('bad', 'bad-repeat'))
            reproduction = 'original issue symptom 2: configuration query' if matched else 'not reproduced'
            excerpts = dict(good_stdout=(case / 'good.stdout').read_text().strip(),
                            bad_stderr=[s for s in (case / 'bad.stderr').read_text().splitlines() if s.startswith('ERROR: No such key')])
        else:
            def cmake(version):
                return str(work / 'tools' / ('cmake-' + version) / 'lib/python3.12/site-packages/cmake/data/bin/cmake')
            specs = [dict(name='new_cmake_executable', path=cmake('4.0.3'), category='Executable changes',
                          kind='exec', operation='execve', left=[], right=['success'], required=True),
                     dict(name='old_cmake_executable', path=cmake('3.31.6'), category='Executable changes',
                          kind='exec', operation='execve', left=['success'], right=[], required=False)]
            matched = all(runs[p]['metadata']['exit_code'] == 0 and (case / 'archives' / p / 'Makefile').is_file() for p in ('good', 'repeat'))
            matched &= all(runs[p]['metadata']['exit_code'] == 1 and 'Compatibility with CMake < 3.5 has been removed from CMake.' in (case / (p + '.stderr')).read_text() for p in ('bad', 'bad-repeat'))
            reproduction = 'original dependency configure failure only; full application not tested' if matched else 'not reproduced'
            excerpts = dict(good_makefile_created=(case / 'archives/good/Makefile').is_file(),
                            bad_stderr=[s.strip() for s in (case / 'bad.stderr').read_text().splitlines()
                                        if s.startswith('CMake Error at') or 'Compatibility with CMake < 3.5 has been removed' in s])
        clues = key_evidence(case, runs['good'], runs['bad'], comparison, specs)
        top_five = any(c['required'] and c['rank'] is not None and c['rank'] <= 5 for c in clues)
        phases = {}
        evidence_rows = []
        for phase, run in runs.items():
            m = run['metadata']
            warnings = []
            for warning in m['warnings']:
                match = re.match(r'raw/trace\.log:(\d+): path context unresolved;', warning)
                category = 'cwd.path_unresolved' if match else 'unclassified'
                row = dict(category=category, text=warning)
                if match:
                    events = [e for e in run['events'] if e['evidence']['line'] == int(match[1]) and e['evidence']['file'] == 'raw/trace.log']
                    assert len(events) == 1
                    event = events[0]
                    row['event'] = {k: event[k] for k in ['kind', 'operation', 'path', 'outcome', 'context_uncertain', 'evidence']}
                    # Preserve the relative spelling; neither classify it as a
                    # physical path nor use this classification to upgrade capture.
                    if event['operation'] == 'access':
                        call = raw_call(case, run, event)
                        mode = re.search(r', (X_OK|R_OK|W_OK|F_OK)\)', call)
                        row['access_mode'] = mode[1] if mode else 'unknown'
                warnings.append(row)
            phases[phase] = dict(state=m['state'], target_exit=m['exit_code'], target_signal=m['signal'],
                                 collector_exit=m['collector_exit_code'], collector_signal=m['collector_signal'],
                                 capture=m['completeness'], storage=run['storage_integrity']['status'],
                                 events=len(run['events']), warnings=warnings,
                                 show_sha256=digest(case / (phase + '.show.json')),
                                 events_sha256=digest(case / 'records/runs' / m['id'] / 'events.jsonl'))
        comparisons = {}
        for label in ('control', 'comparison', 'bad-control'):
            data = read(case / (label + '.json'))
            text = (case / (label + '.txt')).read_text()
            assert f'Exit code: {data["exit_code"]}' in text
            assert ('RELIABILITY NOT ESTABLISHED' in text) == (data['exit_code'] == 3)
            comparisons[label] = dict(observed_differences=data['observed_differences'], changes=len(data['changes']),
                                      exit_code=data['exit_code'], reliability=data['reliability'],
                                      categories=dict(sorted(Counter(c['category'] for c in data['changes']).items())),
                                      warnings=data['warnings'], top5=data['changes'][:5],
                                      full_json_sha256=digest(case / (label + '.json')))
        if name == 'bracket-lib-376':
            events = runs['bad']['events']
            fallback = [e for e in events if e['kind'] == 'exec' and e['path'] == '/usr/bin/as' and e['outcome'] == 'success']
            evidence_rows.append(dict(note='Successful system assembler executions also observed; a preceding PATH ENOENT is not proof the assembler is missing.',
                                      success_count=len(fallback), evidence=[e['evidence'] for e in fallback[:3]]))
        result = dict(schema_version=1, case=name, preregistration_sha256=prereg['case_sha256'][name],
                      original_issue_reproduction=reproduction, target_symptom_matched=bool(matched), control_valid=bool(matched),
                      command=conditions['command'], cwd=conditions['cwd'], selected_startup_environment=[],
                      changed_environment=[dict(key=k, good=good_env.get(k), bad=bad_env.get(k)) for k in differing],
                      source_tools_unchanged=True, fixtures_unchanged=True, phases=phases, comparisons=comparisons,
                      clue_ranks=[dict(name=c['name'], rank=c['rank'], required=c['required']) for c in clues],
                      strict_top5_hit=top_five, conclusion_basis='actual capture; investigator assessment, not a blind user trial',
                      target_output_excerpts=excerpts)
        replacements = [(str(case), '${CASE}'), (str(work), '${WORK}')]
        dest = output / name; dest.mkdir()
        for filename, data in [('summary.json', result), ('evidence.json', dict(key_clues=clues, other_observations=evidence_rows))]:
            (dest / filename).write_text(json.dumps(redact(data, replacements), ensure_ascii=False, indent=2) + '\n')
        print(name, 'symptom', bool(matched), 'top5', top_five, 'ranks', [(c['name'], c['rank']) for c in clues])
    assert all(digest(p) == h for p, h in original_files.items()), 'analysis modified private observations'
    print('PASS: public excerpts generated; all', len(original_files), 'private observation files unchanged')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('work', type=Path)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    analyze(args.work.resolve(), args.output.resolve())
