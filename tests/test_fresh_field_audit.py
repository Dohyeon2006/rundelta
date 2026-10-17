#!/usr/bin/env python3
"""Synthetic audit counterexamples, explicitly not real capture evidence."""
from pathlib import Path
import unittest

from field_warning_contract import audit_warnings as frozen_warnings
from fresh_field_audit import Evidence, audit_clue, audit_normalization_coverage, audit_report, audit_warnings
from fresh_field_contract import audit_command, clue_for, warnings_for


def quoted(value):
    return '"' + ''.join('\\x%02x' % b for b in value.encode()) + '"'


def fixture():
    argv = ['rpm', '--eval', '%{_target_cpu}-%{_vendor}-%{_target_os}%{?_gnu}']
    raw = ['20 execve("/audit/pkg-config", ["pkg-config"], 0) = 0',
           '20 clone(flags=SIGCHLD) = 21',
           '21 execve("/audit/rpm", [' + ', '.join(map(quoted, argv)) + '], 0) = 0',
           '21 access("~/.config/rpm/rpmrc", R_OK) = -1 ENOENT (No such file or directory)',
           '21 +++ exited with 0 +++', '20 +++ exited with 0 +++']
    def event(pid, line, kind, op, outcome, path=None, argv=None, child=None, uncertain=False):
        return dict(pid=pid, kind=kind, operation=op, outcome=outcome, path=path, argv=argv,
                    child_pid=child, cwd_before='/audit/case', context_uncertain=uncertain,
                    evidence=dict(file='raw/trace.log', line=line, end_line=line))
    events = [event('20', 1, 'exec', 'execve', 'success', '/audit/pkg-config', ['pkg-config']),
              event('20', 2, 'spawn', 'clone', 'success', '21', child='21'),
              event('21', 3, 'exec', 'execve', 'success', '/audit/rpm', argv),
              event('21', 4, 'file', 'access', 'ENOENT', 'relative:~/.config/rpm/rpmrc [cwd unresolved]', uncertain=True),
              event('21', 5, 'exit', 'exit', '+++ exited with 0 +++'),
              event('20', 6, 'exit', 'exit', '+++ exited with 0 +++')]
    run = dict(metadata=dict(name='pkg-config-good', command=['pkg-config'], cwd='/audit/case',
                            state='completed', completeness='partial', exit_code=0, collector_exit_code=0,
                            signal=None, collector_signal=None, target_exit_raw='+++ exited with 0 +++',
                            warnings=['raw/trace.log:4: path context unresolved; explanation']),
               storage_integrity=dict(status='verified'), events=events)
    return run, raw


def split(run, raw):
    """Keep exact argument/result bytes; insert another task's signal notification."""
    body, ret = raw[3].split(' = ', 1)
    op = body.split(' ', 1)[1].split('(', 1)[0]
    raw[3:4] = [body + ' <unfinished ...>', '20 --- SIGCHLD {si_signo=SIGCHLD} ---',
                '21 <... ' + op + ' resumed> = ' + ret]
    run['events'][3]['evidence']['end_line'] = 6
    for e in run['events'][4:]:
        e['evidence']['line'] += 2; e['evidence']['end_line'] += 2


def audit_pkg(run, raw):
    return audit_warnings('pkg-config', 'good', Evidence(run, raw))


def covered_file_fixture():
    """An unrelated absolute access still belongs to the raw coverage contract."""
    run, raw = fixture()
    raw.insert(4, '21 openat(AT_FDCWD, "/audit/other.conf", O_RDONLY) = 3')
    for event in run['events'][4:]:
        event['evidence']['line'] += 1
        event['evidence']['end_line'] += 1
    event = dict(run['events'][3], operation='openat', outcome='success',
                 path='/audit/other.conf', context_uncertain=False,
                 evidence=dict(file='raw/trace.log', line=5, end_line=5))
    run['events'].insert(4, event)
    run['metadata'].update(parsed_events=len(run['events']), raw_lines=len(raw))
    return run, raw


def clue_fixture():
    contract = clue_for('gcc-header', Path('/audit'))
    runs = []
    for outcome in ('success', 'ENOENT'):
        run, raw = fixture()
        run['metadata']['command'] = ['gcc', '-c', 'strace.c']
        run['events'][0].update(path='/audit/gcc', argv=['gcc', '-c', 'strace.c'])
        run['events'][2].update(path='/audit/cc1', argv=['cc1', 'strace.c'])
        run['events'][3].update(path=contract['path'], operation='openat', outcome=outcome, context_uncertain=False)
        raw[0] = '20 execve("/audit/gcc", ["gcc", "-c", "strace.c"], 0) = 0'
        raw[2] = '21 execve("/audit/cc1", ["cc1", "strace.c"], 0) = 0'
        ret = '3' if outcome == 'success' else '-1 ENOENT (No such file or directory)'
        raw[3] = '21 openat(AT_FDCWD, ' + quoted(contract['path']) + ', O_RDONLY) = ' + ret
        runs.append((run, raw))
    report = dict(changes=[dict(subject=''), dict(category=contract['category'],
                 subject=contract['path'] + ' (openat)', left=['success'], right=['ENOENT'],
                 left_evidence=[runs[0][0]['events'][3]['evidence'].copy()],
                 right_evidence=[runs[1][0]['events'][3]['evidence'].copy()])])
    return contract, report, runs


class FreshAudit(unittest.TestCase):
    def test_coverage_accepts_every_current_syscall_family(self):
        # Hand-written syscall/result spellings are an independent decoder
        # boundary, not events generated by the product parser under test.
        calls = [
            ('execve', 'exec', '"/audit/tool", ["tool"], 0', '/audit/tool', ['tool']),
            ('execveat', 'exec', 'AT_FDCWD, "/audit/tool", ["tool"], 0, 0', '/audit/tool', ['tool']),
            ('open', 'file', '"/audit/file", O_RDONLY', '/audit/file', None),
            ('openat', 'file', 'AT_FDCWD, "/audit/file", O_RDONLY', '/audit/file', None),
            ('openat2', 'file', 'AT_FDCWD, "/audit/file", {flags=O_RDONLY}, 24', '/audit/file', None),
            ('access', 'file', '"/audit/file", R_OK', '/audit/file', None),
            ('faccessat', 'file', 'AT_FDCWD, "/audit/file", R_OK', '/audit/file', None),
            ('faccessat2', 'file', 'AT_FDCWD, "/audit/file", R_OK, AT_EACCESS', '/audit/file', None),
            ('chdir', 'cwd', '"/audit/directory"', '/audit/directory', None),
            ('fchdir', 'cwd', '3', None, None),
            ('clone', 'spawn', 'flags=SIGCHLD', '22', None),
            ('clone3', 'spawn', '{flags=CLONE_THREAD|CLONE_FS}, 88', '22', None),
            ('fork', 'spawn', '', '22', None),
            ('vfork', 'spawn', '', '22', None),
            ('unshare', 'context', 'CLONE_FS', None, None),
            ('setns', 'context', '3, CLONE_NEWNS', None, None),
            ('chroot', 'context', '"/audit/directory"', None, None),
            ('pivot_root', 'context', '"/audit/new", "/audit/old"', None, None),
            ('mount', 'context', 'NULL, "/audit/mount", NULL, MS_BIND, NULL', None, None),
            ('umount2', 'context', '"/audit/mount", MNT_DETACH', None, None),
            ('move_mount', 'context', '3, "", 4, "", MOVE_MOUNT_F_EMPTY_PATH', None, None),
            ('mount_setattr', 'context', '3, "", AT_EMPTY_PATH, {attr_set=MOUNT_ATTR_RDONLY}, 32', None, None),
        ]
        for operation, kind, args, path, argv in calls:
            for failed in (False, True):
                with self.subTest(operation=operation, failed=failed):
                    run, raw = covered_file_fixture()
                    result = '-1 EACCES (Permission denied)' if failed else ('22' if kind == 'spawn' else '0')
                    raw[4] = f'21 {operation}({args}) = {result}'
                    run['events'][4].update(kind=kind, operation=operation, path=path, argv=argv,
                                           child_pid='22' if kind == 'spawn' and not failed else None,
                                           outcome='EACCES' if failed else 'success')
                    coverage = audit_normalization_coverage(Evidence(run, raw))
                    self.assertEqual(coverage['raw_observations'], len(run['events']))

    def test_noise_exclusion_is_explicit_and_cannot_host_a_normalized_event(self):
        run, raw = fixture()
        split(run, raw)
        coverage = audit_normalization_coverage(Evidence(run, raw))
        self.assertEqual(coverage['excluded'], [dict(line=5, reason='signal delivery is outside normalized Event-v2 families')])
        event = dict(run['events'][3], operation='openat', path='/audit/fake', context_uncertain=False,
                     evidence=dict(file='raw/trace.log', line=5, end_line=5))
        run['events'].append(event)
        with self.assertRaisesRegex(AssertionError, 'normalized event has no raw observation'):
            audit_normalization_coverage(Evidence(run, raw))

    def test_unknown_raw_call_is_not_silently_excluded(self):
        run, raw = covered_file_fixture()
        raw[4] = '21 read(3, "bytes", 5) = 5'
        with self.assertRaisesRegex(AssertionError, 'unsupported raw operation or syntax'):
            audit_normalization_coverage(Evidence(run, raw))

    def test_unfinished_raw_call_without_return_is_rejected(self):
        run, raw = fixture()
        raw.append('20 openat(AT_FDCWD, "/audit/omitted", O_RDONLY <unfinished ...>')
        with self.assertRaisesRegex(AssertionError, 'unfinished raw calls at end of trace'):
            audit_normalization_coverage(Evidence(run, raw))

    def test_wrong_non_clue_result_cannot_pass_coverage(self):
        run, raw = covered_file_fixture()
        run['events'][4]['outcome'] = 'ENOENT'
        with self.assertRaisesRegex(AssertionError, 'syscall/result mismatch'):
            audit_pkg(run, raw)

    def test_missing_child_exit_and_split_supported_access_are_rejected(self):
        for deleted in ('exit', 'split-file'):
            with self.subTest(deleted=deleted):
                run, raw = fixture()
                if deleted == 'split-file':
                    split(run, raw)
                    del run['events'][3]
                else:
                    del run['events'][4]
                with self.assertRaisesRegex(AssertionError, 'raw observation missing normalized event'):
                    audit_normalization_coverage(Evidence(run, raw))

    def test_unrelated_supported_raw_access_is_also_audited(self):
        run, raw = covered_file_fixture()
        self.assertEqual(audit_pkg(run, raw)['warning_count'], 1)

    def test_missing_normalized_absolute_openat_is_rejected(self):
        run, raw = covered_file_fixture()
        del run['events'][4]
        run['metadata']['parsed_events'] = len(run['events'])
        with self.assertRaisesRegex(AssertionError, 'raw observation missing normalized event'):
            audit_pkg(run, raw)

    def test_report_only_cannot_accept_the_same_omission_on_both_sides(self):
        run, raw = covered_file_fixture()
        run['metadata']['id'] = 'a-20'
        warnings = audit_pkg(run, raw)
        del run['events'][4]
        run['metadata']['parsed_events'] = len(run['events'])
        evidence = Evidence(run, raw)
        msg = 'pkg-config-good: state=completed, 1 parser warnings; inspect metadata.json'
        report = dict(schema_version=4, errors=[], observed_differences=False, changes=[], exit_code=3,
                      left='pkg-config-good', right='pkg-config-good', left_id='a-20', right_id='a-20',
                      reliability={side: dict(capture='incomplete', storage='verified') for side in ('left', 'right')},
                      warnings=[msg, msg])
        text = ('Observed differences: no\nExit code: 3\nStorage integrity: left=verified, right=verified\n'
                'Capture completeness: left=incomplete, right=incomplete\nRELIABILITY NOT ESTABLISHED\nTotal observed changes: 0')
        with self.assertRaisesRegex(AssertionError, 'raw observation missing normalized event'):
            audit_report(report, text, 3, 3, evidence, evidence, warnings, warnings)

    def test_single_line_passes_both_contracts(self):
        run, raw = fixture()
        frozen_warnings('pkg-config', 'good', run, raw)
        result = audit_pkg(run, raw)
        self.assertEqual(result['expected'], result['actual'])

    def test_old_contract_rejects_same_semantic_split(self):
        run, raw = fixture(); split(run, raw)
        with self.assertRaises(AssertionError):
            frozen_warnings('pkg-config', 'good', run, raw)

    def test_split_and_interleaving_pass_new_contract(self):
        run, raw = fixture(); split(run, raw)
        result = audit_pkg(run, raw)
        self.assertEqual(result['expected'], result['actual'])
        self.assertEqual(result['split_warning_count'], 1)

    def test_line_changes_with_correct_semantics_pass(self):
        run, raw = fixture()
        raw.insert(3, '20 --- SIGCHLD {si_signo=SIGCHLD} ---')
        for e in run['events'][3:]:
            e['evidence']['line'] += 1; e['evidence']['end_line'] += 1
        run['metadata']['warnings'][0] = 'raw/trace.log:5: path context unresolved; relocated source'
        self.assertEqual(audit_pkg(run, raw)['warning_count'], 1)

    def test_wrong_syscall_rejected_even_with_coherent_record(self):
        run, raw = fixture()
        run['events'][3]['operation'] = 'faccessat'
        raw[3] = '21 faccessat(AT_FDCWD, "~/.config/rpm/rpmrc", R_OK) = -1 ENOENT (No such file or directory)'
        with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_wrong_outcome_rejected_even_with_coherent_record(self):
        run, raw = fixture()
        run['events'][3]['outcome'] = 'EACCES'; raw[3] = raw[3].replace('ENOENT', 'EACCES')
        with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_wrong_path_rejected_even_with_coherent_record(self):
        run, raw = fixture()
        run['events'][3]['path'] = 'relative:~/.config/rpm/other [cwd unresolved]'
        raw[3] = raw[3].replace('rpmrc', 'other')
        with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_wrong_path_semantics_rejected(self):
        for path, uncertain in [('/audit/case/~/.config/rpm/rpmrc', False),
                                ('relative:~/.config/rpm/rpmrc [dirfd unresolved]', True),
                                ('relative:~/.config/rpm/rpmrc [cwd unresolved]', False)]:
            with self.subTest(path=path, uncertain=uncertain):
                run, raw = fixture(); run['events'][3].update(path=path, context_uncertain=uncertain)
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_wrong_process_role_rejected_with_matching_pid_evidence(self):
        run, raw = fixture()
        run['events'][3]['pid'] = '20'; raw[3] = raw[3].replace('21 access', '20 access')
        with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_wrong_parent_and_exec_evidence_rejected(self):
        for change in ('parent', 'argv', 'executable', 'raw-argv', 'raw-child'):
            with self.subTest(change=change):
                run, raw = fixture()
                if change == 'parent': run['events'][1]['pid'] = '99'; raw[1] = raw[1].replace('20 clone', '99 clone')
                if change == 'argv': run['events'][2]['argv'] = ['rpm', '--version']
                if change == 'executable': run['events'][2]['path'] = '/audit/unrelated'
                if change == 'raw-argv': raw[2] = '21 execve("/audit/rpm", ["rpm", "--version"], 0) = 0'
                if change == 'raw-child': raw[1] = raw[1].replace('= 21', '= 29')
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_extra_warning_categories_or_occurrences_rejected(self):
        extras = ['raw/trace.log:4: unsupported or incomplete event: ???',
                  'raw/trace.log:4: unfinished call at end of trace',
                  'cannot read trace: failure', 'collector status does not confirm target completion',
                  'process completion not observed for PIDs: 21', 'storage checksum mismatch',
                  'raw/trace.log:4: fs.context: unknown clone flags',
                  'shared filesystem state or root/namespace changes observed; cwd fallback disabled',
                  'raw/trace.log:4: path context unresolved; duplicate',
                  'raw/trace.log:3: path context unresolved; different event']
        for extra in extras:
            with self.subTest(extra=extra):
                run, raw = fixture(); run['metadata']['warnings'].append(extra)
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_same_line_numbers_wrong_raw_semantics_rejected(self):
        for replacement in ('EACCES', 'other-path', 'wrong-pid', 'wrong-mode', 'wrong-op'):
            with self.subTest(replacement=replacement):
                run, raw = fixture()
                old, new = {'EACCES': ('ENOENT', 'EACCES'), 'other-path': ('rpmrc', 'other'),
                            'wrong-pid': ('21 access', '29 access'), 'wrong-mode': ('R_OK', 'X_OK'),
                            'wrong-op': ('access(', 'open(')}[replacement]
                raw[3] = raw[3].replace(old, new)
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_wrong_resumed_pair_rejected(self):
        for change in ('syscall', 'pid', 'result', 'same-pid-call', 'no-entry', 'different-end'):
            with self.subTest(change=change):
                run, raw = fixture(); split(run, raw)
                if change == 'syscall': raw[5] = raw[5].replace('access resumed', 'openat resumed')
                if change == 'pid': raw[5] = raw[5].replace('21 <', '20 <')
                if change == 'result': raw[5] = raw[5].replace('ENOENT', 'EACCES')
                if change == 'same-pid-call': raw[4] = '21 access("other", R_OK) = 0'
                if change == 'no-entry': raw[3] = raw[3].replace('<unfinished ...>', '= 0')
                if change == 'different-end': run['events'][3]['evidence']['end_line'] = 5
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_missing_warning_and_uncovered_uncertainty_rejected(self):
        for change in ('missing', 'extra-event', 'context'):
            with self.subTest(change=change):
                run, raw = fixture()
                if change == 'missing': run['metadata']['warnings'] = []
                elif change == 'context': run['events'][1].update(kind='context', context_uncertain=True)
                else: run['events'][-1]['context_uncertain'] = True
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_reliability_storage_and_exit_contracts_rejected(self):
        for field, value in [('state', 'interrupted'), ('completeness', 'complete_for_supported_events'),
                             ('collector_exit_code', 9), ('collector_signal', 15), ('signal', 15),
                             ('exit_code', 2), ('target_exit_raw', '+++ killed by SIGTERM +++')]:
            with self.subTest(field=field):
                run, raw = fixture(); run['metadata'][field] = value
                with self.assertRaises(AssertionError): audit_pkg(run, raw)
        for status in ('corrupt', 'unverified', 'unfinalized'):
            with self.subTest(status=status):
                run, raw = fixture(); run['storage_integrity']['status'] = status
                with self.assertRaises(AssertionError): audit_pkg(run, raw)

    def test_zero_warning_cases_do_not_accept_old_cargo_warning(self):
        for name in ('python-venv', 'gcc-header', 'cargo-linker'):
            for phase in ('good', 'repeat', 'bad'):
                self.assertEqual(warnings_for(name, phase), {})
        run, raw = fixture()
        run['metadata']['warnings'] = ['shared filesystem state or root/namespace changes observed; cwd fallback disabled']
        with self.assertRaises(AssertionError): audit_warnings('cargo-linker', 'good', Evidence(run, raw))

    def test_rank_and_clue_contract_passes(self):
        contract, report, runs = clue_fixture()
        self.assertEqual(audit_clue(contract, report, *[Evidence(*r) for r in runs]), 2)

    def test_top_five_is_not_relaxed_for_unfixed_ranks(self):
        contract, report, runs = clue_fixture()
        contract['exact_rank'] = None
        report['changes'][0:0] = [dict(subject='noise')] * 4
        with self.assertRaises(AssertionError): audit_clue(contract, report, *[Evidence(*r) for r in runs])

    def test_rustc_role_requires_proven_thread_edges(self):
        def cargo():
            run, _ = fixture()
            def e(pid, line, kind, operation, path, argv=None, child=None, outcome='success'):
                row = dict(run['events'][0], pid=pid, kind=kind, operation=operation, path=path,
                           argv=argv, child_pid=child, outcome=outcome,
                           evidence=dict(file='raw/trace.log', line=line, end_line=line))
                return row
            run['metadata']['command'] = ['cargo', 'build']
            run['events'] = [e('20', 1, 'exec', 'execve', '/audit/cargo', ['cargo', 'build']),
                             e('20', 2, 'spawn', 'clone3', '21', child='21'),
                             e('21', 3, 'spawn', 'clone', '22', child='22'),
                             e('22', 4, 'exec', 'execve', '/audit/rustc', ['rustc', '--crate-name', 'build']),
                             e('22', 5, 'spawn', 'clone3', '23', child='23'),
                             e('23', 6, 'spawn', 'vfork', '24', child='24'),
                             e('24', 7, 'exec', 'execve', '/audit/missing-linker', ['/audit/missing-linker'], outcome='ENOENT')]
            raw = ['20 execve("/audit/cargo", ["cargo", "build"], 0) = 0',
                   '20 clone3({flags=CLONE_THREAD|CLONE_FS}, 88) = 21',
                   '21 clone(flags=SIGCHLD) = 22',
                   '22 execve("/audit/rustc", ["rustc", "--crate-name", "build"], 0) = 0',
                   '22 clone3({flags=CLONE_THREAD|CLONE_FS}, 88) = 23',
                   '23 vfork() = 24',
                   '24 execve("/audit/missing-linker", ["/audit/missing-linker"], 0) = -1 ENOENT (No such file or directory)']
            return run, raw
        run, raw = cargo()
        self.assertEqual(Evidence(run, raw).actor(run['events'][-1]), 'rustc_linker_child')
        raw[4] = raw[4].replace('CLONE_THREAD|', '')
        with self.assertRaises(AssertionError): Evidence(run, raw).actor(run['events'][-1])
        run, raw = cargo()
        run['events'][3]['path'] = '/audit/other'; raw[3] = raw[3].replace('/audit/rustc', '/audit/other')
        self.assertEqual(Evidence(run, raw).actor(run['events'][-1]), 'other')

    def test_report_exit_reliability_and_text_must_agree(self):
        # A self comparison isolates the reliability gate from change ranking.
        run, raw = fixture(); run['metadata']['id'] = 'a-20'
        evidence = Evidence(run, raw)
        warnings = audit_pkg(run, raw)
        msg = 'pkg-config-good: state=completed, 1 parser warnings; inspect metadata.json'
        def data():
            return dict(schema_version=4, errors=[], observed_differences=False, changes=[], exit_code=3,
                        left='pkg-config-good', right='pkg-config-good', left_id='a-20', right_id='a-20',
                        reliability={side: dict(capture='incomplete', storage='verified') for side in ('left', 'right')},
                        warnings=[msg, msg])
        text = ('Observed differences: no\nExit code: 3\nStorage integrity: left=verified, right=verified\n'
                'Capture completeness: left=incomplete, right=incomplete\nRELIABILITY NOT ESTABLISHED\nTotal observed changes: 0')
        audit_report(data(), text, 3, 3, evidence, evidence, warnings, warnings)
        for change in ('json-rc', 'cli-rc', 'text-rc', 'capture', 'storage', 'extra-warning', 'text-status', 'json-observed'):
            with self.subTest(change=change):
                d, t, rc, text_rc = data(), text, 3, 3
                if change == 'json-rc': d['exit_code'] = 0
                if change == 'cli-rc': rc = 0
                if change == 'text-rc': text_rc = 0
                if change == 'capture': d['reliability']['left']['capture'] = 'complete'
                if change == 'storage': d['reliability']['right']['storage'] = 'unverified'
                if change == 'extra-warning': d['warnings'].append('unknown event')
                if change == 'text-status': t = t.replace('left=incomplete', 'left=complete')
                if change == 'json-observed': d['observed_differences'] = True
                with self.assertRaises(AssertionError): audit_report(d, t, rc, text_rc, evidence, evidence, warnings, warnings)

    def test_changed_workload_command_is_rejected(self):
        audit_command('pkg-config', ['pkg-config', '--cflags', '--libs', 'libcrypt'], Path('/audit'), Path('/project'))
        with self.assertRaises(AssertionError):
            audit_command('pkg-config', ['pkg-config', '--version'], Path('/audit'), Path('/project'))

    def test_clue_line_changes_with_split_event_pass(self):
        contract, report, runs = clue_fixture()
        for side, (run, raw) in zip(('left', 'right'), runs):
            split(run, raw)
            report['changes'][1][side + '_evidence'] = [run['events'][3]['evidence'].copy()]
        self.assertEqual(audit_clue(contract, report, *[Evidence(*r) for r in runs]), 2)

    def test_wrong_clue_category_outcome_path_role_and_rank_rejected(self):
        for change in ('category', 'outcome', 'path', 'role', 'missing', 'rank', 'missing-evidence'):
            with self.subTest(change=change):
                contract, report, runs = clue_fixture()
                c = report['changes'][1]
                if change == 'category': c['category'] = 'New file access'
                if change == 'outcome': c['right'] = ['EACCES']
                if change == 'path': c['subject'] = c['subject'].replace('defs.h', 'other.h')
                if change == 'role':
                    runs[1][0]['events'][3]['pid'] = '20'; runs[1][1][3] = runs[1][1][3].replace('21 openat', '20 openat')
                if change == 'missing': report['changes'] = report['changes'][:1]
                if change == 'rank': report['changes'].insert(0, dict(subject='unrelated'))
                if change == 'missing-evidence': c['right_evidence'] = []
                with self.assertRaises(AssertionError): audit_clue(contract, report, *[Evidence(*r) for r in runs])

    def test_syscall_argument_delimiters_are_not_split(self):
        run, raw = fixture()
        operand = 'nested, ) = 0 [file]'
        raw[3] = '21 access(' + quoted(operand) + ', R_OK) = -1 ENOENT (No such file or directory)'
        run['events'][3]['path'] = 'relative:' + operand + ' [cwd unresolved]'
        Evidence(run, raw).validate(run['events'][3])
        with self.assertRaises(AssertionError): audit_pkg(run, raw)  # correct decoding, wrong registered path

    def test_descriptor_attributes_do_not_invent_path_evidence(self):
        run, raw = fixture()
        event = run['events'][3]
        encoded = quoted('/dev/null')[1:-1]
        event.update(operation='openat', outcome='success', path='/dev/null', context_uncertain=False)
        raw[3] = f'21 openat(AT_FDCWD, "/dev/null", O_RDWR) = 3<{encoded}<char 1:3>>'
        Evidence(run, raw).validate(event)
        raw[3] = raw[3].replace('"/dev/null"', '"null"')
        with self.assertRaises(AssertionError): Evidence(run, raw).validate(event)


if __name__ == '__main__':
    unittest.main(verbosity=2)
