#!/usr/bin/env python3
"""Exact warning contracts; mutations must fail closed, independently of captures."""
import copy
from pathlib import Path
import textwrap
import unittest
from field_warning_contract import audit_warnings

WARNING = 'raw/trace.log:4: path context unresolved; last observed CWD is not a verified current path; consult raw evidence'

def fixture():
    operand = '~/.config/rpm/rpmrc'
    quoted = ''.join('\\x%02x' % b for b in operand.encode())
    raw = ['20 execve("/synthetic/pkg-config", [], 0) = 0',
           '20 clone() = 21',
           '21 execve("/synthetic/rpm", [], 0) = 0',
           f'21 access("{quoted}", R_OK) = -1 ENOENT (No such file or directory)']
    evidence = lambda line: dict(file='raw/trace.log', line=line, end_line=line)
    events = [dict(pid='20', kind='exec', operation='execve', outcome='success', path='/synthetic/pkg-config', argv=['pkg-config'], child_pid=None, context_uncertain=False, cwd_before='/synthetic/case', evidence=evidence(1)),
              dict(pid='21', kind='file', operation='access', outcome='ENOENT', path='relative:'+operand+' [cwd unresolved]', argv=None, child_pid=None, context_uncertain=True, cwd_before='/synthetic/case', evidence=evidence(4))]
    events[1:1] = [dict(events[0], kind='spawn', operation='clone', path='21', child_pid='21', argv=None, evidence=evidence(2)),
                   dict(events[0], pid='21', path='/synthetic/rpm', argv=['rpm','--eval','%{_target_cpu}-%{_vendor}-%{_target_os}%{?_gnu}'], evidence=evidence(3))]
    return dict(metadata=dict(name='pkg-config-good',cwd='/synthetic/case', warnings=[WARNING], state='completed', completeness='partial', exit_code=0, signal=None, target_exit_raw='+++ exited with 0 +++', collector_exit_code=0, collector_signal=None),events=events,storage_integrity=dict(status='verified')), raw

class Contracts(unittest.TestCase):
    def test_frozen_v1_rejects_the_same_expected_pkg_warning(self):
        old=(Path(__file__).parent/'fixtures/field_report_audit_v1.py').read_text()
        block=old[old.index("        warnings = run['metadata']['warnings']"):old.index("    if name == 'gcc-header':")]
        run,_=fixture()
        with self.assertRaises(AssertionError):
            exec(compile(textwrap.dedent(block), '<frozen-v1-warning-gate>', 'exec'), dict(name='pkg-config',phase='good',run=run))

    def test_rpm_origin_requires_the_declared_child_and_exec(self):
        for change in ['wrong-parent','wrong-argv','wrong-executable','root-access']:
            with self.subTest(change=change):
                run,raw=fixture()
                if change=='wrong-parent':run['events'][1]['pid']='99'
                if change=='wrong-argv':run['events'][2]['argv']=['rpm','--version']
                if change=='wrong-executable':run['events'][2]['path']='/synthetic/other'
                if change=='root-access':
                    run['events'][-1]['pid']='20';raw[-1]=raw[-1].replace('21 access','20 access')
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)

    def test_cargo_global_warning_is_bounded_to_shared_clone_evidence(self):
        base,raw=fixture()
        base['metadata']['warnings']=['shared filesystem state or root/namespace changes observed; cwd fallback disabled']
        base['events']=base['events'][:2]
        for e in base['events']:e['cwd_before']=None
        base['events'][1].update(context_uncertain=True,operation='clone3')
        raw=raw[:1]+['20 clone3({flags=CLONE_VM|CLONE_FS}, 88) = 21']
        report=audit_warnings('cargo-linker','good',base,raw)
        self.assertEqual(report['expected'],report['actual'])
        self.assertEqual(report['actual'][0]['count'],1)
        for change in ['duplicate','extra-warning','missing-warning','missing-flag','context-event','unrelated-parent']:
            with self.subTest(change=change):
                run=copy.deepcopy(base);lines=raw.copy()
                if change=='duplicate':run['metadata']['warnings']*=2
                if change=='extra-warning':run['metadata']['warnings'].append(WARNING)
                if change=='missing-warning':run['metadata']['warnings']=[]
                if change=='missing-flag':lines[1]=lines[1].replace('CLONE_FS','CLONE_FILES')
                if change=='context-event':run['events'][1]['kind']='context'
                if change=='unrelated-parent':
                    run['events'][1]['pid']='99';lines[1]=lines[1].replace('20 clone3','99 clone3')
                with self.assertRaises(AssertionError):audit_warnings('cargo-linker','good',run,lines)

    def test_expected_pkg_warning_is_accepted_and_shown(self):
        run,raw=fixture()
        result=audit_warnings('pkg-config','good',run,raw)
        self.assertEqual(result['expected'], result['actual'])
        self.assertEqual(result['actual'][0]['type'],'cwd.relative_access')
        self.assertEqual(result['actual'][0]['count'],1)
        self.assertEqual(result['warnings'],[WARNING])

    def test_line_numbers_and_explanatory_prose_are_not_the_contract(self):
        run,raw=fixture()
        raw.insert(3,'20 +++ unrelated evidence line +++')
        run['events'][-1]['evidence'].update(line=5,end_line=5)
        run['metadata']['warnings']=['raw/trace.log:5: path context unresolved; explanation may be reworded']
        result=audit_warnings('pkg-config','good',run,raw)
        self.assertEqual(result['expected'],result['actual'])

    def test_all_pkg_phases_have_exactly_one_warning(self):
        for phase in ['good','repeat','bad']:
            with self.subTest(phase=phase):
                run,raw=fixture();code=1 if phase=='bad' else 0
                run['metadata'].update(exit_code=code,collector_exit_code=code,target_exit_raw=f'+++ exited with {code} +++')
                audit_warnings('pkg-config',phase,run,raw)

    def test_extra_warning_classes_are_rejected(self):
        extras=['raw/trace.log:4: unsupported or incomplete event: access(...)',
                'cannot read trace: failure','unrecognized target termination signal',
                'process completion not observed for PIDs: 21',
                'collector status does not confirm target completion',
                'storage checksum mismatch',
                'shared filesystem state or root/namespace changes observed; cwd fallback disabled',
                'raw/trace.log:4: execution arguments incomplete',
                'raw/trace.log:4: another CWD warning',WARNING,
                'raw/trace.log:1: path context unresolved; another source']
        for warning in extras:
            with self.subTest(warning=warning):
                run,raw=fixture();run['metadata']['warnings'].append(warning)
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)

    def test_missing_replaced_or_unlocated_warning_is_rejected(self):
        for warnings in [[],['collector failed'],[WARNING.replace('raw/trace.log','other.log')],
                         [WARNING.replace(':4:',':99:')],[WARNING.replace(':4:',':1:')]]:
            with self.subTest(warnings=warnings):
                run,raw=fixture();run['metadata']['warnings']=warnings
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)

    def test_wrong_event_semantics_are_rejected(self):
        for field,value in [('pid','22'),('kind','cwd'),('operation','openat'),('path','relative:other [cwd unresolved]'),('outcome','EACCES'),('cwd_before','/other'),('context_uncertain',False)]:
            with self.subTest(field=field):
                run,raw=fixture();run['events'][-1][field]=value
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)

    def test_wrong_or_missing_raw_evidence_is_rejected(self):
        for raw_change in [[],['unrelated','20 access("other", R_OK) = -1 ENOENT'],
                           ['unrelated','20 access("~/.config/rpm/rpmrc", X_OK) = -1 ENOENT']]:
            with self.subTest(raw=raw_change):
                run,_=fixture()
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw_change)

    def test_unknown_event_or_duplicate_uncertainty_cannot_hide(self):
        for change in ['duplicate','context','unflagged']:
            with self.subTest(change=change):
                run,raw=fixture();extra=copy.deepcopy(run['events'][-1])
                if change=='context':extra.update(kind='context',operation='unshare',context_uncertain=True)
                if change=='unflagged':extra.update(path='relative:other [cwd unresolved]',context_uncertain=False)
                run['events'].append(extra)
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)

    def test_capture_storage_and_exit_failures_are_rejected(self):
        changes=[('state','capture_failed'),('state','interrupted'),('completeness','complete_for_supported_events'),
                 ('collector_exit_code',9),('collector_signal',15),('signal',15),('exit_code',2)]
        for field,value in changes:
            with self.subTest(field=field,value=value):
                run,raw=fixture();run['metadata'][field]=value
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)
        for status in ['corrupt','unverified','unfinalized']:
            with self.subTest(status=status):
                run,raw=fixture();run['storage_integrity']['status']=status
                with self.assertRaises(AssertionError):audit_warnings('pkg-config','good',run,raw)

    def test_undeclared_cases_remain_zero_warning(self):
        for name in ['python-venv','gcc-header','new-case']:
            run,raw=fixture()
            with self.assertRaises(AssertionError):audit_warnings(name,'good',run,raw)
        run,raw=fixture();run['metadata'].update(warnings=[],completeness='complete_for_supported_events')
        run['events']=run['events'][:1]
        result=audit_warnings('python-venv','good',run,raw)
        self.assertEqual(result['actual'],[])

if __name__=='__main__':unittest.main(verbosity=2)
