#!/usr/bin/env python3
"""Real CLI regressions; mutate only freshly captured records in a new directory."""
import errno, hashlib, json, os, pathlib, resource, shutil, subprocess, sys, tempfile, time, unittest
from unittest import mock
from acceptance_support import main, strace_environment
from capture_owner_support import FaultBroker
ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = pathlib.Path(os.environ.get('RUNDELTA_BIN', ROOT/'target/debug/rundelta')).resolve()
ENV = strace_environment(ROOT)
WORK = pathlib.Path(tempfile.mkdtemp(prefix='storage-cli-', dir=ROOT/'target'))
LOG = []

def call(store, *args, **kwargs):
    def limit_fds():
        resource.setrlimit(resource.RLIMIT_NOFILE, (kwargs['nofile'], kwargs['nofile']))
    p = subprocess.run([str(BIN), '--storage', str(store), *args], env=ENV,
                       stderr=subprocess.PIPE, stdout=kwargs.get('stdout', subprocess.PIPE), timeout=15,
                       preexec_fn=limit_fds if 'nofile' in kwargs else None)
    LOG.append(dict(args=args, returncode=p.returncode, stderr=p.stderr.decode(errors='replace')))
    return p

class StorageCLI(unittest.TestCase):
    def setUp(self):
        self.store=WORK/self._testMethodName
        p=call(self.store,'record','healthy','--','/usr/bin/true')
        self.assertEqual(p.returncode,0,p.stderr)
        self.good=next((self.store/'runs').iterdir())

    def damage(self, mode):
        p=call(self.store,'record','damaged','--','/usr/bin/true');self.assertEqual(p.returncode,0,p.stderr)
        d=next(d for d in (self.store/'runs').iterdir() if d!=self.good)
        if mode=='missing_metadata': (d/'metadata.json').unlink()
        elif mode=='bad_metadata': (d/'metadata.json').write_text('{broken')
        elif mode=='unreadable': (d/'metadata.json').chmod(0)
        elif mode=='no_marker': (d/'finalized.json').unlink()
        elif mode=='checksum':
            p=d/'events.jsonl';p.write_bytes(p.read_bytes().replace(b'success',b'succesz',1))
        return d

    def synthetic_record(self, label):
        """Copy a new capture for negative controls; never touch historical fixtures."""
        rid=f'{time.time_ns():x}-{os.getpid()}'
        directory=self.store/'runs'/rid
        shutil.copytree(self.good,directory)
        metadata=json.loads((directory/'metadata.json').read_bytes())
        metadata.update(id=rid,name=label)
        (directory/'metadata.json').write_text(json.dumps(metadata))
        self.refresh_marker(directory)
        return directory

    def refresh_marker(self, directory):
        marker=json.loads((directory/'finalized.json').read_bytes())
        marker['run_id']=directory.name
        for key,filename in [('metadata','metadata.json'),('events','events.jsonl')]:
            data=(directory/filename).read_bytes()
            marker[key]={'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()}
        marker['event_count']=len((directory/'events.jsonl').read_bytes().splitlines())
        (directory/'finalized.json').write_text(json.dumps(marker))

    def replace_events(self, directory, events):
        metadata=json.loads((directory/'metadata.json').read_bytes())
        metadata['parsed_events']=len(events)
        (directory/'metadata.json').write_text(json.dumps(metadata))
        data=b''.join((json.dumps(event,separators=(',',':'))+'\n').encode() for event in events)
        (directory/'events.jsonl').write_bytes(data)
        self.refresh_marker(directory)

    def record_trace_v1(self, label, command=('/usr/bin/true',), expected=0):
        """Exercise the explicit contract without changing default capture fixtures."""
        recorded=call(self.store,'record',label,'--evidence-contract','trace-v1','--',*command)
        self.assertEqual(recorded.returncode,expected,recorded.stderr+recorded.stdout)
        shown=call(self.store,'show',label,'--json')
        self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
        run=json.loads(shown.stdout)
        return self.store/'runs'/run['metadata']['id'],run

    def trace_v1_copy(self, label):
        """Mutate copies of a fresh opt-in capture; preserve all frozen evidence."""
        if not hasattr(self,'trace_template'):
            self.trace_template,_=self.record_trace_v1('trace-template')
        rid=f'{time.time_ns():x}-{os.getpid()}'
        directory=self.store/'runs'/rid
        shutil.copytree(self.trace_template,directory)
        metadata=json.loads((directory/'metadata.json').read_bytes())
        metadata.update(id=rid,name=label)
        (directory/'metadata.json').write_text(json.dumps(metadata))
        manifest=json.loads((directory/'raw-integrity.json').read_bytes())
        manifest['run_id']=rid
        (directory/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(directory)
        return directory

    def refresh_trace_v1_marker(self, directory):
        """Rebind structured test bytes, never conceal a raw-byte mutation."""
        self.refresh_marker(directory)
        marker=json.loads((directory/'finalized.json').read_bytes())
        data=(directory/'raw-integrity.json').read_bytes()
        marker['raw_manifest']={'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest()}
        (directory/'finalized.json').write_text(json.dumps(marker))

    def trace_v5_report(self, left, right, expected, explicit=False):
        """Check JSON truth and the same process status in both renderings."""
        version=('--report-version','5') if explicit else ()
        reports=[]
        for suffix in [(),('--json',)]:
            compared=call(self.store,'diff',left,right,*version,*suffix)
            self.assertEqual(compared.returncode,expected,compared.stderr+compared.stdout)
            if suffix:
                report=json.loads(compared.stdout)
                self.assertEqual(report['schema_version'],5,report)
                self.assertEqual(report['exit_code'],expected,report)
                reports.append(report)
        return reports[0]

    def assert_trace_v1_wire(self, directory, run):
        """Independently relate persisted provenance, normalized events and raw bytes."""
        metadata=run['metadata']
        events=run['events']
        self.assertEqual(metadata['schema_version'],4,metadata)
        self.assertEqual(metadata['evidence_contract'],'trace-v1',metadata)
        self.assertTrue(events)
        self.assertTrue(all(event['schema_version']==3 for event in events),events)
        for event in events:
            self.assertIsInstance(event['task_generation'],int,event)
            self.assertGreater(event['task_generation'],0,event)
            if event['kind']=='spawn' and event['outcome']=='success':
                self.assertIsInstance(event['child_generation'],int,event)
                self.assertGreater(event['child_generation'],0,event)
            else:self.assertIsNone(event.get('child_generation'),event)
        marker=json.loads((directory/'finalized.json').read_bytes())
        self.assertEqual(marker['schema_version'],2,marker)
        manifest_bytes=(directory/'raw-integrity.json').read_bytes()
        self.assertEqual(marker['raw_manifest'],{
            'bytes':len(manifest_bytes),'sha256':hashlib.sha256(manifest_bytes).hexdigest(),
        })
        manifest=json.loads(manifest_bytes)
        self.assertEqual(manifest['schema_version'],1,manifest)
        self.assertEqual(manifest['run_id'],metadata['id'],manifest)
        self.assertEqual(manifest['file'],'raw/trace.log',manifest)
        raw=(directory/'raw/trace.log').read_bytes()
        self.assertEqual(manifest['receipt'],{
            'status':'parsed','bytes':len(raw),'logical_lines':len(raw.splitlines()),
            'sha256':hashlib.sha256(raw).hexdigest(),
        },manifest)
        self.assertEqual(run['storage_integrity']['status'],'verified',run)
        self.assertEqual(run['raw_integrity']['status'],'verified',run)
        self.assertEqual(run['raw_integrity']['manifest'],manifest,run)
        provenance=metadata['target_provenance']
        self.assertEqual(provenance['schema_version'],1,provenance)
        self.assertEqual(provenance['selection_rule'],'first_successful_exec_evidence_order_v1')
        self.assertEqual(provenance['identity_contract'],'task_generations_evidence_order_v1')
        references=[]
        for field in ['root_exec','target_exit']:
            reference=provenance[field]
            event=events[reference['index']]
            self.assertEqual(reference['evidence'],event['evidence'],reference)
            self.assertEqual(event['pid'],provenance['root']['pid'],event)
            self.assertEqual(event['task_generation'],provenance['root']['generation'],event)
            references.append(event)
        self.assertEqual(references[0]['kind'],'exec',references)
        self.assertEqual(references[0]['outcome'],'success',references)
        self.assertEqual(references[1]['kind'],'exit',references)
        self.assertEqual(references[1]['outcome'],metadata['target_exit_raw'],references)
        evidence=references[1]['evidence']
        self.assertEqual(evidence['line'],evidence['end_line'],evidence)
        self.assertTrue(raw.splitlines()[evidence['line']-1].endswith(metadata['target_exit_raw'].encode()))

    def synthetic_trace_v1(self, label, reused_pid=False):
        """Build transparent current-contract controls, not rewritten legacy runs.

        Reuse deliberately has the same PID and exit text on both generations;
        the stored generation and event references must select the second exit.
        Its identity warning keeps this artificial reuse example partial.
        """
        directory=self.trace_v1_copy(label)
        raw_lines=[]
        events=[]
        def append(kind,operation,outcome,generation,path=None,argv=None,uncertain=False):
            line=len(raw_lines)+1
            raw_lines.append('41 '+(outcome if kind=='exit' else
                'execve("/usr/bin/true", ["/usr/bin/true"], 0x1 /* 0 vars */) = 0'))
            events.append(dict(
                schema_version=3,pid='41',kind=kind,operation=operation,path=path,
                outcome=outcome,evidence=dict(file='raw/trace.log',line=line,end_line=line),
                argv=argv,child_pid=None,cwd_before=None,context_uncertain=uncertain,
                task_generation=generation,child_generation=None,
            ))
        if reused_pid:append('exit','exit','+++ exited with 0 +++',1)
        generation=2 if reused_pid else 1
        append('exec','execve','success',generation,'/usr/bin/true',['/usr/bin/true'],reused_pid)
        append('exit','exit','+++ exited with 0 +++',generation,uncertain=reused_pid)
        raw=('\n'.join(raw_lines)+'\n').encode()
        (directory/'raw/trace.log').write_bytes(raw)
        metadata=json.loads((directory/'metadata.json').read_bytes())
        root_exec=len(events)-2
        target_exit=len(events)-1
        metadata.update(command=['/usr/bin/true'],state='completed',exit_code=0,signal=None,
                        collector_exit_code=0,collector_signal=None,
                        target_exit_raw='+++ exited with 0 +++',parsed_events=len(events),
                        raw_lines=len(raw_lines),warnings=['synthetic PID reuse identity limitation'] if reused_pid else [],
                        completeness='partial' if reused_pid else 'complete_for_supported_events')
        metadata['target_provenance']=dict(
            schema_version=1,selection_rule='first_successful_exec_evidence_order_v1',
            identity_contract='task_generations_evidence_order_v1',
            root=dict(pid='41',generation=generation),
            root_exec=dict(index=root_exec,evidence=events[root_exec]['evidence']),
            target_exit=dict(index=target_exit,evidence=events[target_exit]['evidence']),
        )
        (directory/'metadata.json').write_text(json.dumps(metadata))
        (directory/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
        manifest=json.loads((directory/'raw-integrity.json').read_bytes())
        manifest['receipt']=dict(status='parsed',bytes=len(raw),logical_lines=len(raw_lines),
                                 sha256=hashlib.sha256(raw).hexdigest())
        (directory/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(directory)
        return directory

    def synthetic_trace_v1_outcome(self, label, outcome, code=None, signal=None):
        """Keep normalized/raw references consistent while varying numeric facts."""
        directory=self.synthetic_trace_v1(label)
        metadata=json.loads((directory/'metadata.json').read_bytes())
        events=[json.loads(line) for line in (directory/'events.jsonl').read_bytes().splitlines()]
        events[-1]['outcome']=outcome
        known=code is not None or signal is not None
        metadata.update(target_exit_raw=outcome,exit_code=code,signal=signal,
                        collector_exit_code=code if known else 0,collector_signal=signal,
                        completeness='complete_for_supported_events' if known else 'partial',
                        warnings=[] if known else ['synthetic unknown target exit spelling'])
        (directory/'metadata.json').write_text(json.dumps(metadata))
        (directory/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
        raw=(directory/'raw/trace.log').read_bytes().splitlines()
        raw[-1]=b'41 '+outcome.encode()
        raw=b'\n'.join(raw)+b'\n'
        (directory/'raw/trace.log').write_bytes(raw)
        manifest=json.loads((directory/'raw-integrity.json').read_bytes())
        manifest['receipt'].update(bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest())
        (directory/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(directory)
        return directory

    def synthetic_mapping_run(self, label, observations, pid='41'):
        """Build saved DTO controls with a bound raw receipt, not parser evidence.

        Each supplied dictionary describes one file/exec occurrence. Optional
        coordinates permit vector order, resumed spans and duplicate-coordinate
        controls. Those synthetic DTOs test the saved-reference contract; their
        checksum does not certify that a parser would produce this event vector.
        """
        directory=self.synthetic_trace_v1(label)
        metadata=json.loads((directory/'metadata.json').read_bytes())
        initial=[json.loads(line) for line in (directory/'events.jsonl').read_bytes().splitlines()]
        events=[initial[0]]
        for index,specification in enumerate(observations,2):
            start,end=specification.get('coordinates',(index,index))
            self.assertGreaterEqual(start,2)
            self.assertGreaterEqual(end,start)
            kind=specification.get('kind','file')
            operation=specification.get('operation','execve' if kind=='exec' else 'openat')
            events.append(dict(
                schema_version=3,pid=pid,kind=kind,operation=operation,path=specification['path'],
                outcome=specification['outcome'],evidence=dict(file='raw/trace.log',line=start,end_line=end),
                argv=[specification['path']] if kind=='exec' else None,child_pid=None,
                cwd_before=None,context_uncertain=False,task_generation=1,child_generation=None,
            ))
        last_line=max(event['evidence']['end_line'] for event in events)+1
        final=initial[-1]
        final['evidence'].update(line=last_line,end_line=last_line)
        events.append(final)
        for event in [events[0],events[-1]]:event['pid']=pid
        # Raw framing and line counts are independently verifiable even when a
        # control deliberately gives multiple DTO occurrences one coordinate.
        rows=[pid+' getpid() = '+pid for _ in range(last_line)]
        rows[0]=pid+' execve("/usr/bin/true", ["/usr/bin/true"], 0x1 /* 0 vars */) = 0'
        rows[-1]=pid+' '+final['outcome']
        for event in events[1:-1]:
            path=json.dumps(event['path'])
            result='3<'+event['path']+'>' if event['outcome']=='success' else '-1 '+event['outcome']+' (synthetic result)'
            syscall=(event['operation']+'('+path+', ['+path+'], 0x1 /* 0 vars */)' if event['kind']=='exec'
                     else event['operation']+'(AT_FDCWD, '+path+', O_RDONLY)')
            start,end=event['evidence']['line'],event['evidence']['end_line']
            rows[start-1]=pid+' '+syscall+(' = '+result if start==end else ' <unfinished ...>')
            if start!=end:rows[end-1]=pid+' <... '+event['operation']+' resumed> ) = '+result
        raw=('\n'.join(rows)+'\n').encode()
        (directory/'raw/trace.log').write_bytes(raw)
        metadata.update(parsed_events=len(events),raw_lines=len(rows))
        metadata['target_provenance']['root']['pid']=pid
        metadata['target_provenance']['target_exit']=dict(index=len(events)-1,evidence=final['evidence'])
        (directory/'metadata.json').write_text(json.dumps(metadata))
        (directory/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
        manifest=json.loads((directory/'raw-integrity.json').read_bytes())
        manifest['receipt'].update(bytes=len(raw),logical_lines=len(rows),sha256=hashlib.sha256(raw).hexdigest())
        (directory/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(directory)
        return directory

    def mapping_run(self, directory):
        """Use the public reader as the independent saved-vector oracle."""
        shown=call(self.store,'show',directory.name,'--json')
        self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
        return json.loads(shown.stdout)

    def mapping_call(self, *arguments):
        """Retain both raw output streams even when a v6 assertion fails."""
        result=call(self.store,*arguments)
        prefix=WORK/('v6-command-'+str(len(LOG)))
        for name,payload in [('stdout',result.stdout),('stderr',result.stderr)]:
            path=prefix.with_suffix('.'+name)
            path.write_bytes(payload)
            LOG[-1][name+'_file']=str(path)
            LOG[-1][name+'_sha256']=hashlib.sha256(payload).hexdigest()
        return result

    def trace_v6_report(self, left, right, expected, withheld_target=None):
        """Require v6 parity and the frozen v5 projection, with one named boundary.

        V6 deliberately withholds an unfinalized side's target samples that v5
        historically exposed from metadata alone. The exception must identify
        that side and its exact saved coordinate; every other v5 field is equal.
        """
        text=self.mapping_call('diff',left,right,'--report-version','6')
        self.assertEqual(text.returncode,expected,text.stderr+text.stdout)
        self.assertIn(b'outcome_event_references_v1',text.stdout)
        self.assertIn(('Exit code: '+str(expected)).encode(),text.stdout)
        result=self.mapping_call('diff',left,right,'--report-version','6','--json')
        self.assertEqual(result.returncode,expected,result.stderr+result.stdout)
        report=json.loads(result.stdout)
        self.assertEqual(report['schema_version'],6,report)
        self.assertEqual(report['evidence_contract'],'outcome_event_references_v1',report)
        self.assertEqual(report['exit_code'],expected,report)
        projected=json.loads(result.stdout)
        projected['schema_version']=5
        projected.pop('evidence_contract')
        for change in projected['changes']:change.pop('evidence_mapping')
        older=self.mapping_call('diff',left,right,'--report-version','5','--json')
        self.assertEqual(older.returncode,expected,older.stderr+older.stdout)
        old_report=json.loads(older.stdout)
        if withheld_target is not None:
            side,evidence=withheld_target
            self.assertIn(side,['left','right'])
            change=next(change for change in projected['changes'] if change['category']=='Exit status')
            old_change=next(change for change in old_report['changes'] if change['category']=='Exit status')
            self.assertEqual(change[side+'_evidence'],[])
            self.assertEqual(old_change[side+'_evidence'],[evidence])
            change[side+'_evidence']=[evidence]
        self.assertEqual(projected,old_report)
        return report

    def assert_v6_normalized_mapping(self, change, left, right):
        """Rebuild full outcome references from typed keys and saved events only."""
        mapping=change['evidence_mapping']
        self.assertEqual(set(mapping),{'observation','sample_limit_per_side','left','right'})
        key=mapping['observation']
        self.assertEqual(set(key),{'kind','operation','path'})
        self.assertEqual(mapping['sample_limit_per_side'],3)
        for side,run in [('left',left),('right',right)]:
            if run['storage_integrity']['status']=='unfinalized':
                self.assert_v6_metadata_side(change,side,[],source='withheld')
                self.assertEqual(change[side],[])
                continue
            matching=[(index,event) for index,event in enumerate(run['events'])
                      if all(event[field]==key[field] for field in ['kind','operation','path'])]
            outcomes=sorted({event['outcome'] for _,event in matching})
            expected=[]
            for outcome in outcomes:
                references=[dict(index=index,evidence=event['evidence']) for index,event in matching
                            if event['outcome']==outcome]
                expected.append(dict(outcome=outcome,event_count=len(references),events=references))
            facts=mapping[side]
            self.assertEqual(set(facts),{'source','metadata_fields','event_count','sample_event_indices',
                                        'sample_count','omitted_sample_count','outcomes'})
            self.assertEqual(facts['source'],'normalized_events')
            self.assertEqual(facts['metadata_fields'],[])
            self.assertEqual(facts['event_count'],len(matching))
            self.assertEqual(facts['outcomes'],expected)
            self.assertEqual(change[side],outcomes)
            samples=facts['sample_event_indices']
            self.assertEqual(facts['sample_count'],len(samples))
            self.assertLessEqual(len(samples),3)
            self.assertEqual(len(samples),len(set(samples)))
            self.assertTrue(set(samples).issubset({index for index,_ in matching}))
            self.assertEqual(change[side+'_evidence'],[run['events'][index]['evidence'] for index in samples])
            self.assertEqual(facts['omitted_sample_count'],len(matching)-len(samples))

    def assert_v6_metadata_side(self, change, side, fields, source='metadata'):
        """Metadata or withheld facts must not manufacture event attribution."""
        mapping=change['evidence_mapping']
        self.assertEqual(mapping['sample_limit_per_side'],3)
        self.assertEqual(mapping[side],dict(source=source,metadata_fields=fields,event_count=None,
                         sample_event_indices=[],sample_count=0,omitted_sample_count=None,outcomes=[]))
        self.assertEqual(change[side+'_evidence'],[])

    def assert_v6_target_side(self, change, side, run):
        """Bind an observed target comparison value to its saved exit reference."""
        reference=run['metadata']['target_provenance']['target_exit']
        event=run['events'][reference['index']]
        self.assertEqual(event['kind'],'exit')
        self.assertEqual(reference['evidence'],event['evidence'])
        fields=['metadata.exit_code','metadata.signal','metadata.target_exit_raw']
        self.assertEqual(change['evidence_mapping']['observation'],None)
        self.assertEqual(change['evidence_mapping'][side],dict(source='target_provenance',metadata_fields=fields,
                         event_count=1,sample_event_indices=[reference['index']],sample_count=1,
                         omitted_sample_count=0,outcomes=[dict(outcome=change[side][0],event_count=1,events=[reference])]))
        self.assertEqual(change[side+'_evidence'],[event['evidence']])

    def assert_failed_record_isolated(self, directory, status):
        listing=call(self.store,'list','--json')
        self.assertEqual(listing.returncode,0,listing.stderr)
        rows=json.loads(listing.stdout)
        bad=next(row for row in rows if row['id']==directory.name)
        good=next(row for row in rows if row['id']==self.good.name)
        self.assertEqual(bad['storage_integrity']['status'],status,bad)
        self.assertEqual(good['storage_integrity']['status'],'verified',good)
        for suffix in [(),('--json',)]:
            self.assertEqual(call(self.store,'list',*suffix).returncode,0)
            self.assertEqual(call(self.store,'show','healthy',*suffix).returncode,0)
            self.assertEqual(call(self.store,'diff','healthy','healthy',*suffix).returncode,0)
            shown=call(self.store,'show',directory.name,*suffix)
            self.assertEqual(shown.returncode,2,shown.stderr+shown.stdout)
            self.assertNotIn(b'panicked',shown.stderr)
            compared=call(self.store,'diff',directory.name,'healthy',*suffix)
            self.assertEqual(compared.returncode,2,compared.stderr+compared.stdout)
            if suffix:
                report=json.loads(compared.stdout)
                self.assertIsNone(report['observed_differences'])
                self.assertEqual(report['changes'],[])
                self.assertEqual(report['reliability']['left']['storage'],status)
        return bad

    def test_resource_limits_isolate_metadata_marker_and_event_line(self):
        # The limits are public storage-operation policy, independent of schema
        # damage: even matching marker hashes do not make an over-budget record readable.
        cases=['metadata','marker','event-line']
        for case in cases:
            with self.subTest(case=case):
                directory=self.synthetic_record('over-budget-'+case)
                if case=='metadata':
                    (directory/'metadata.json').write_bytes(b'{"name":"'+b'x'*(8*1024*1024)+b'"}')
                    self.refresh_marker(directory)
                elif case=='marker':
                    with (directory/'finalized.json').open('wb') as marker:
                        marker.truncate(64*1024+1)
                else:
                    event=json.loads((self.good/'events.jsonl').read_bytes().splitlines()[0])
                    event['outcome']='x'*(8*1024*1024)
                    self.replace_events(directory,[event])
                row=self.assert_failed_record_isolated(directory,'unavailable')
                self.assertIn('resource limit exceeded',row['storage_integrity']['detail'])

    def test_json_container_budgets_reject_small_but_allocation_heavy_input(self):
        for case in ['metadata','event']:
            with self.subTest(case=case):
                directory=self.synthetic_record('many-json-values-'+case)
                if case=='metadata':
                    metadata=json.loads((directory/'metadata.json').read_bytes())
                    metadata['environment']={f'V{index}':None for index in range(40_000)}
                    (directory/'metadata.json').write_text(json.dumps(metadata))
                    self.refresh_marker(directory)
                else:
                    event=json.loads((self.good/'events.jsonl').read_bytes().splitlines()[0])
                    event.update(kind='exec',operation='execve',path='/usr/bin/true',argv=['']*65_536,child_pid=None)
                    self.replace_events(directory,[event])
                row=self.assert_failed_record_isolated(directory,'unavailable')
                self.assertIn('JSON values',row['storage_integrity']['detail'])

    def test_large_valid_event_remains_readable_within_the_budget(self):
        directory=self.synthetic_record('large-valid')
        event=json.loads((self.good/'events.jsonl').read_bytes().splitlines()[0])
        event['outcome']='x'*(1024*1024)
        self.replace_events(directory,[event])
        shown=call(self.store,'show','large-valid','--json')
        self.assertEqual(shown.returncode,0,shown.stderr)
        run=json.loads(shown.stdout)
        self.assertEqual(run['storage_integrity']['status'],'verified')
        self.assertEqual(run['events'][0]['outcome'],event['outcome'])
        self.assertEqual(call(self.store,'diff','large-valid','large-valid','--json').returncode,0)

    def test_matching_hashes_cannot_hide_pathless_current_file_or_exec(self):
        for kind in ['file','exec']:
            controls=[]
            for outcome in ['success','ENOENT']:
                with self.subTest(kind=kind,outcome=outcome):
                    directory=self.synthetic_record('invalid-'+kind+'-'+outcome)
                    event=json.loads((self.good/'events.jsonl').read_bytes().splitlines()[0])
                    event.update(kind=kind,operation='openat' if kind=='file' else 'execve',
                                 path=None,outcome=outcome,argv=None,child_pid=None)
                    self.replace_events(directory,[event])
                    row=self.assert_failed_record_isolated(directory,'corrupt')
                    self.assertIn('nonempty observed path',row['storage_integrity']['detail'])
                    controls.append(directory)
            compared=call(self.store,'diff',controls[0].name,controls[1].name,'--json')
            self.assertEqual(compared.returncode,2,compared.stderr+compared.stdout)
            self.assertIsNone(json.loads(compared.stdout)['observed_differences'])

    def test_complete_information_gaps_fail_and_partial_observations_remain_readable(self):
        for kind,operation,path,argv in [
            ('exec','execve','/usr/bin/true',None),
            ('cwd','fchdir',None,None),
            ('file','openat','relative:input [cwd unresolved]',None),
            ('exec','execve','relative:input [cwd unresolved]',['input']),
            ('cwd','chdir','relative:input [cwd unresolved]',None),
        ]:
            with self.subTest(kind=kind,operation=operation,path=path):
                directory=self.synthetic_record('information-gap-'+kind+'-'+operation)
                event=json.loads((self.good/'events.jsonl').read_bytes().splitlines()[0])
                event.update(kind=kind,operation=operation,path=path,argv=argv,child_pid=None,
                             outcome='success',context_uncertain=False)
                self.replace_events(directory,[event])
                row=self.assert_failed_record_isolated(directory,'corrupt')
                needs_uncertainty=path is None or not path.startswith('/')
                if not needs_uncertainty:
                    self.assertIn('without decoded argv',row['storage_integrity']['detail'])

                for completeness in ['partial','incomplete']:
                    metadata=json.loads((directory/'metadata.json').read_bytes())
                    metadata['completeness']=completeness
                    metadata['warnings']=['synthetic information gap']
                    if completeness=='incomplete':
                        metadata.update(state='capture_failed',exit_code=None,collector_exit_code=None,
                                        target_exit_raw=None)
                    (directory/'metadata.json').write_text(json.dumps(metadata))
                    self.refresh_marker(directory)
                    if needs_uncertainty:
                        event['context_uncertain']=False
                        self.replace_events(directory,[event])
                        self.assert_failed_record_isolated(directory,'corrupt')
                    event['context_uncertain']=needs_uncertainty
                    self.replace_events(directory,[event])
                    for suffix in [(),('--json',)]:
                        shown=call(self.store,'show',directory.name,*suffix)
                        self.assertEqual(shown.returncode,0,shown.stderr)
                        if suffix:
                            run=json.loads(shown.stdout)
                            self.assertEqual(run['storage_integrity']['status'],'verified')
                            self.assertEqual(run['events'][0]['path'],path)
                            self.assertEqual(run['events'][0]['argv'],argv)
                            self.assertEqual(run['events'][0]['context_uncertain'],needs_uncertainty)
                        compared=call(self.store,'diff',directory.name,directory.name,*suffix)
                        self.assertEqual(compared.returncode,3,compared.stderr+compared.stdout)

    def test_legacy_shapes_are_not_reinterpreted_by_current_contract(self):
        for version in [1,2]:
            with self.subTest(version=version):
                directory=self.synthetic_record('legacy-shape-'+str(version))
                event=json.loads((self.good/'events.jsonl').read_bytes().splitlines()[0])
                event.update(kind='file',operation='openat',path=None,argv=None,child_pid=None)
                self.replace_events(directory,[event])
                metadata=json.loads((directory/'metadata.json').read_bytes())
                metadata['schema_version']=version
                if version==1:
                    metadata.pop('environment',None);metadata.pop('completeness',None)
                (directory/'metadata.json').write_text(json.dumps(metadata))
                self.refresh_marker(directory)
                shown=call(self.store,'show',directory.name,'--json')
                self.assertEqual(shown.returncode,0,shown.stderr)
                run=json.loads(shown.stdout)
                self.assertEqual(run['storage_integrity']['status'],'unverified')
                self.assertIsNone(run['events'][0]['path'])
                self.assertEqual(call(self.store,'diff',directory.name,directory.name,'--json').returncode,3)

    def populate_healthy_records(self, count):
        template=json.loads((self.good/'metadata.json').read_bytes())
        marker=json.loads((self.good/'finalized.json').read_bytes())
        labels=['healthy']
        for index in range(1,count):
            rid=f'a{index:08x}-101';label=f'bounded-index-{index:03d}'
            directory=self.store/'runs'/rid
            shutil.copytree(self.good,directory)
            metadata=dict(template,id=rid,name=label)
            data=json.dumps(metadata).encode()
            (directory/'metadata.json').write_bytes(data)
            finalized=dict(marker,run_id=rid,metadata={
                'bytes':len(data),'sha256':hashlib.sha256(data).hexdigest(),
            })
            (directory/'finalized.json').write_text(json.dumps(finalized))
            labels.append(label)
        return labels

    def test_large_index_bounds_fds_and_preserves_healthy_reads(self):
        labels=self.populate_healthy_records(70)
        listing=call(self.store,'list','--json',nofile=64)
        self.assertEqual(listing.returncode,0,listing.stderr)
        rows=json.loads(listing.stdout)
        self.assertEqual(len(rows),70)
        self.assertEqual({row['name'] for row in rows},set(labels))
        self.assertTrue(all(row['storage_integrity']['status']=='verified' for row in rows),rows)
        for label in labels:
            shown=call(self.store,'show',label,'--json',nofile=64)
            self.assertEqual(shown.returncode,0,shown.stderr)
            self.assertEqual(json.loads(shown.stdout)['storage_integrity']['status'],'verified')

    def test_large_index_keeps_every_label_reserved_under_low_fd_limit(self):
        labels=self.populate_healthy_records(70)
        before={directory.name for directory in (self.store/'runs').iterdir()}
        for label in reversed(labels):
            attempted=call(self.store,'record',label,'--','/usr/bin/true',nofile=64)
            self.assertEqual(attempted.returncode,2,attempted.stderr)
            self.assertIn(b'record name already exists',attempted.stderr)
            self.assertEqual({directory.name for directory in (self.store/'runs').iterdir()},before)
        rows=json.loads(call(self.store,'list','--json').stdout)
        self.assertEqual(len({row['name'] for row in rows}),70)

    def test_index_io_failures_block_record_without_side_effects(self):
        self.assertNotEqual(os.geteuid(),0,'permission regression requires non-root')
        damaged=self.damage('no_marker')
        for index,path in enumerate([damaged/'metadata.json',damaged,self.store/'runs']):
            with self.subTest(path=path.name):
                before={directory.name for directory in (self.store/'runs').iterdir()}
                target_ran=WORK/(self._testMethodName+f'-{index}.target-ran')
                original_mode=path.stat().st_mode & 0o777
                path.chmod(0)
                try:
                    attempted=call(self.store,'record',f'must-not-be-created-{index}','--',
                                   '/usr/bin/touch',str(target_ran))
                    self.assertEqual(attempted.returncode,2,attempted.stderr)
                    self.assertIn(b'Permission denied',attempted.stderr)
                    self.assertNotIn(b'corrupt',attempted.stderr)
                finally:
                    path.chmod(original_mode)
                self.assertFalse(target_ran.exists())
                self.assertEqual({directory.name for directory in (self.store/'runs').iterdir()},before)

    def test_index_fd_exhaustion_blocks_record_without_side_effects(self):
        before={directory.name for directory in (self.store/'runs').iterdir()}
        target_ran=WORK/(self._testMethodName+'.target-ran')
        for limit in [7,8]:
            with self.subTest(nofile=limit):
                attempted=call(self.store,'record','fd-exhausted','--',
                               '/usr/bin/touch',str(target_ran),nofile=limit)
                self.assertEqual(attempted.returncode,2,attempted.stderr)
                self.assertIn(b'Too many open files',attempted.stderr)
                self.assertNotIn(b'corrupt',attempted.stderr)
                self.assertFalse(target_ran.exists())
                self.assertEqual({directory.name for directory in (self.store/'runs').iterdir()},before)

    def test_broken_pipe(self):
        for cmd in [('show','healthy'),('list',),('diff','healthy','healthy')]:
            for suffix in [(),('--json',)]:
                with self.subTest(command=cmd+suffix):
                    r,w=os.pipe();os.close(r)
                    try:p=call(self.store,*cmd,*suffix,stdout=w)
                    finally:os.close(w)
                    self.assertEqual(p.returncode,0,p.stderr)
                    self.assertNotIn(b'panicked',p.stderr)

    def test_non_pipe_output_failure(self):
        for suffix in [(),('--json',)]:
            with self.subTest(suffix=suffix),open('/dev/full','wb') as sink:
                p=call(self.store,'show','healthy',*suffix,stdout=sink)
                self.assertEqual(p.returncode,2,p.stderr)
                self.assertIn(b'stdout',p.stderr)
                self.assertNotIn(b'corrupt',p.stderr)
                self.assertNotIn(b'panicked',p.stderr)

    def test_record_stderr_contract_and_failures(self):
        recorded=call(self.store,'record','summary','--','/usr/bin/true')
        self.assertEqual(recorded.returncode,0,recorded.stderr)
        self.assertEqual(recorded.stdout,b'')
        shown=call(self.store,'show','summary','--json')
        self.assertEqual(shown.returncode,0,shown.stderr)
        run=json.loads(shown.stdout);metadata=run['metadata']
        expected=(
            f'Recorded "summary": {metadata["id"]} '
            f'({metadata["state"]}, {metadata["parsed_events"]} events, '
            f'{len(metadata["warnings"])} warnings)\n'
            f'Evidence: {self.store/"runs"/metadata["id"]}\n'
            + ''.join(f'warning: {warning}\n' for warning in metadata['warnings'])
        ).encode()
        self.assertEqual(recorded.stderr,expected)

        r,w=os.pipe();os.close(r)
        try:
            piped=subprocess.run(
                [str(BIN),'--storage',str(self.store),'record','stderr-pipe','--','/usr/bin/true'],
                env=ENV,stdout=subprocess.PIPE,stderr=w,timeout=15,
            )
        finally:os.close(w)
        self.assertEqual(piped.returncode,0,piped.stdout)
        piped_run=call(self.store,'show','stderr-pipe','--json')
        self.assertEqual(piped_run.returncode,0,piped_run.stderr)
        self.assertEqual(json.loads(piped_run.stdout)['storage_integrity']['status'],'verified')

        with open('/dev/full','wb') as sink:
            full=subprocess.run(
                [str(BIN),'--storage',str(self.store),'record','stderr-full','--','/usr/bin/true'],
                env=ENV,stdout=subprocess.PIPE,stderr=sink,timeout=15,
            )
        self.assertEqual(full.returncode,2,full.stdout)
        full_run=call(self.store,'show','stderr-full','--json')
        self.assertEqual(full_run.returncode,0,full_run.stderr)
        self.assertEqual(json.loads(full_run.stdout)['storage_integrity']['status'],'verified')

    def test_isolation_matrix(self):
        self.assertNotEqual(os.geteuid(),0,'permission regression requires non-root')
        for mode in ['missing_metadata','bad_metadata','unreadable','no_marker','checksum']:
            with self.subTest(mode=mode):
                d=self.damage(mode)
                try:
                    before={str(p.relative_to(d)):hashlib.sha256(p.read_bytes()).hexdigest() for p in d.rglob('*') if p.is_file()} if mode!='unreadable' else None
                    for cmd in [('show','healthy','--json'),('diff','healthy','healthy','--json')]:
                        p=call(self.store,*cmd);self.assertEqual(p.returncode,0,p.stderr+p.stdout)
                    listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
                    row=next(r for r in json.loads(listing.stdout) if r['id']==d.name)
                    expected_status='unfinalized' if mode=='no_marker' else ('unavailable' if mode=='unreadable' else 'corrupt')
                    self.assertEqual(row['storage_integrity']['status'],expected_status)
                    self.assertEqual(row['directory'],'runs/'+d.name)
                    self.assertIn(d.name.encode(),call(self.store,'list').stdout)
                    show=call(self.store,'show',d.name,'--json')
                    self.assertEqual(show.returncode,0 if mode=='no_marker' else 2)
                    diff=call(self.store,'diff',d.name,'healthy','--json')
                    self.assertEqual(diff.returncode,3 if mode=='no_marker' else 2)
                    if mode!='no_marker':
                        report=json.loads(diff.stdout)
                        self.assertIsNone(report['observed_differences'])
                        self.assertEqual(report['reliability']['left']['storage'],'unavailable' if mode=='unreadable' else 'corrupt')
                    p=call(self.store,'record','fresh-'+mode,'--','/usr/bin/true')
                    self.assertEqual(p.returncode,2 if mode=='unreadable' else 0,p.stderr)
                    if mode!='unreadable':
                        self.assertEqual(call(self.store,'delete','fresh-'+mode).returncode,0)
                    if before is not None:self.assertEqual(before,{str(p.relative_to(d)):hashlib.sha256(p.read_bytes()).hexdigest() for p in d.rglob('*') if p.is_file()})
                    self.assertEqual(call(self.store,'delete',d.name).returncode,0)
                    self.assertFalse(d.exists())
                finally:
                    if d.exists():
                        (d/'metadata.json').chmod(0o600) if (d/'metadata.json').exists() else None
                        shutil.rmtree(d)

    def test_named_damage_and_duplicate_label(self):
        d=self.damage('checksum')
        self.assertEqual(call(self.store,'record','damaged','--','/usr/bin/true').returncode,2)
        no_probe=ENV.copy();no_probe['RUNDELTA_STRACE']=str(WORK/'missing-strace-probe')
        duplicate=subprocess.run(
            [str(BIN),'--storage',str(self.store),'record','damaged','--','/usr/bin/true'],
            env=no_probe,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15,
        )
        self.assertEqual(duplicate.returncode,2,duplicate.stderr)
        self.assertIn(b'record name already exists',duplicate.stderr)
        self.assertNotIn(b'strace --version',duplicate.stderr)
        self.assertEqual(call(self.store,'show','damaged').returncode,2)
        self.assertEqual(call(self.store,'delete','damaged').returncode,0)
        self.assertFalse(d.exists())

    def test_fixed_metadata_temp_hardlink_is_ignored_without_external_write(self):
        ready=WORK/(self._testMethodName+'.ready')
        release=WORK/(self._testMethodName+'.release')
        sentinel=WORK/(self._testMethodName+'.sentinel')
        target=(
            'import pathlib,sys,time\n'
            'ready=pathlib.Path(sys.argv[1]);release=pathlib.Path(sys.argv[2])\n'
            'ready.write_text("ready")\n'
            'deadline=time.monotonic()+10\n'
            'while not release.exists():\n'
            '    if time.monotonic() >= deadline: sys.exit(124)\n'
            '    time.sleep(0.01)\n'
        )
        args=('record','metadata-hardlink','--',sys.executable,'-c',target,str(ready),str(release))
        cmd=[str(BIN),'--storage',str(self.store),*args]
        p=subprocess.Popen(cmd,env=ENV,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        stdout=stderr=b''
        try:
            deadline=time.monotonic()+10
            while not ready.exists():
                if p.poll() is not None:
                    stdout,stderr=p.communicate()
                    self.fail(f'waiting target exited early: rc={p.returncode}, stderr={stderr!r}')
                self.assertLess(time.monotonic(),deadline,'waiting target readiness timeout')
                time.sleep(0.01)
            runs=[d for d in (self.store/'runs').iterdir() if d.is_dir() and d!=self.good]
            self.assertEqual(len(runs),1,runs)
            directory=runs[0]
            self.assertTrue((directory/'metadata.json').is_file())
            self.assertFalse((directory/'metadata.tmp').exists())
            sentinel.write_bytes(b'external sentinel must remain unchanged')
            os.link(sentinel,directory/'metadata.tmp')

            release.write_text('release')
            stdout,stderr=p.communicate(timeout=15)
            self.assertEqual(p.returncode,0,stderr+stdout)
            self.assertEqual(sentinel.read_bytes(),b'external sentinel must remain unchanged')
            self.assertEqual((directory/'metadata.tmp').read_bytes(),b'external sentinel must remain unchanged')
            self.assertNotEqual((directory/'metadata.json').stat().st_ino,sentinel.stat().st_ino)
            self.assertTrue((directory/'finalized.json').is_file())
            shown=call(self.store,'show','metadata-hardlink','--json')
            self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
            self.assertEqual(json.loads(shown.stdout)['storage_integrity']['status'],'verified')
        finally:
            release.touch(exist_ok=True)
            if p.poll() is None:
                try:stdout,stderr=p.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    p.kill();stdout,stderr=p.communicate(timeout=5)
            LOG.append(dict(args=args,returncode=p.returncode,stderr=stderr.decode(errors='replace')))

    def test_runs_symlink_rejects_all_storage_entry_points(self):
        runs=self.store/'runs'
        saved=self.store/'runs.safe'
        outside=WORK/(self._testMethodName+'-outside')
        external_record=outside/'abc-123'
        sentinel=external_record/'sentinel'
        side_effect=WORK/(self._testMethodName+'.target-ran')
        runs.rename(saved)
        external_record.mkdir(parents=True)
        sentinel.write_text('keep')
        runs.symlink_to(outside,target_is_directory=True)
        try:
            cases=[
                ('list',('list','--json')),
                ('load',('show','abc-123','--json')),
                ('record',('record','blocked-by-runs-symlink','--','/usr/bin/touch',str(side_effect))),
                ('delete',('delete','abc-123')),
            ]
            for operation,args in cases:
                with self.subTest(operation=operation):
                    result=call(self.store,*args)
                    self.assertEqual(result.returncode,2,result.stderr+result.stdout)
                    self.assertIn(b'unsafe storage root',result.stderr)
                    self.assertTrue(sentinel.is_file())
            self.assertFalse(side_effect.exists())
        finally:
            if runs.is_symlink():runs.unlink()
            if saved.exists() and not runs.exists():saved.rename(runs)

    def test_ambiguous_and_unsafe(self):
        d=self.damage('no_marker')
        m=json.loads((d/'metadata.json').read_text());m['name']='healthy';(d/'metadata.json').write_text(json.dumps(m))
        for cmd in [('show','healthy'),('delete','healthy'),('diff','healthy','healthy')]:
            self.assertEqual(call(self.store,*cmd).returncode,2)
        self.assertTrue(self.good.exists());self.assertTrue(d.exists())
        self.assertEqual(call(self.store,'delete',d.name).returncode,0)
        invalid=self.store/'runs'/'not-a-run-id';invalid.mkdir()
        sentinel=invalid/'keep';sentinel.write_text('keep')
        self.assertEqual(call(self.store,'delete','not-a-run-id').returncode,2)
        self.assertTrue(sentinel.exists())
        self.assertEqual(call(self.store,'show',self.good.name).returncode,0)
        listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
        row=next(r for r in json.loads(listing.stdout) if r['directory']=='runs/not-a-run-id')
        self.assertIsNone(row['id']);self.assertEqual(row['storage_integrity']['status'],'unavailable')
        link=self.store/'runs'/'abc-123';link.symlink_to(self.good,target_is_directory=True)
        self.assertEqual(call(self.store,'delete','abc-123').returncode,2)
        self.assertTrue(self.good.exists())

    def test_root_symlink_trailing_slashes_reject_every_entry_point(self):
        linked=WORK/(self._testMethodName+'-linked')
        linked.symlink_to(self.store,target_is_directory=True)
        target_ran=WORK/(self._testMethodName+'.target-ran')
        before=(self.good/'metadata.json').read_bytes()
        for suffix in ['', '/', '///']:
            for args in [('list','--json'),('show','healthy','--json'),
                         ('diff','healthy','healthy'),('diff','healthy','healthy','--json'),('delete',self.good.name),
                         ('record','blocked','--','/usr/bin/touch',str(target_ran))]:
                with self.subTest(suffix=suffix,args=args):
                    result=call(str(linked)+suffix,*args)
                    self.assertEqual(result.returncode,2,result.stderr+result.stdout)
                    self.assertIn(b'unsafe storage root',result.stderr+result.stdout)
                    if args[0]=='diff' and args[-1]=='--json':
                        report=json.loads(result.stdout)
                        self.assertEqual(report['exit_code'],2)
                        self.assertIsNone(report['observed_differences'])
                        self.assertEqual(report['reliability']['left']['storage'],'unavailable')
                    self.assertEqual((self.good/'metadata.json').read_bytes(),before)
                    self.assertFalse(target_ran.exists())
        for suffix in ['/', '///']:
            result=call(str(self.store)+suffix,'diff','healthy','healthy','--json')
            self.assertEqual(result.returncode,0,result.stderr+result.stdout)
            self.assertEqual(json.loads(result.stdout)['exit_code'],0)

    def test_delete_deep_tree_returns_tool_error_instead_of_stack_abort(self):
        # dir_fd construction avoids PATH_MAX; low stack makes the regression
        # deterministic without constructing thousands of unrelated path names.
        victim=self.store/'runs/abc-1'
        victim.mkdir()
        first_evidence=victim/'first-evidence'
        first_evidence.write_bytes(b'preserve before deep child')
        frames=[os.open(victim,os.O_RDONLY|os.O_DIRECTORY|os.O_CLOEXEC)]
        try:
            for _ in range(256):
                os.mkdir('n',dir_fd=frames[-1])
                try:
                    child=os.open('n',os.O_RDONLY|os.O_DIRECTORY|os.O_CLOEXEC,
                                  dir_fd=frames[-1])
                except OSError:
                    # mkdir succeeded without a held child fd. Remove that
                    # empty child before unwinding the tracked ancestor chain.
                    try:os.rmdir('n',dir_fd=frames[-1])
                    except OSError:pass
                    raise
                frames.append(child)
            sentinel=os.open('sentinel',os.O_WRONLY|os.O_CREAT|os.O_EXCL,
                             0o600,dir_fd=frames[-1])
            with os.fdopen(sentinel,'wb') as output:output.write(b'deep evidence')
            def bounded_stack():
                resource.setrlimit(resource.RLIMIT_STACK,(512*1024,512*1024))
                resource.setrlimit(resource.RLIMIT_CORE,(0,0))
            result=subprocess.run([str(BIN),'--storage',str(self.store),'delete','abc-1'],
                                  env=ENV,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                                  preexec_fn=bounded_stack,timeout=15)
            LOG.append(dict(args=['delete','abc-1','stack-limit=512KiB'],
                            returncode=result.returncode,stderr=result.stderr.decode(errors='replace')))
            self.assertEqual(result.returncode,2,result.stderr+result.stdout)
            self.assertIn(b'resource limit exceeded (directory depth: 128)',result.stderr)
            self.assertNotIn(b'Deleted',result.stdout)
            self.assertTrue(victim.is_dir())
            self.assertEqual(first_evidence.read_bytes(),b'preserve before deep child')
            sentinel=os.open('sentinel',os.O_RDONLY,dir_fd=frames[-1])
            with os.fdopen(sentinel,'rb') as source:self.assertEqual(source.read(),b'deep evidence')
            self.assertEqual(call(self.store,'show','healthy','--json').returncode,0)
            self.assertIn(b'before removal during preflight',result.stderr)
            self.assertNotIn(b'may be partially deleted',result.stderr)
        finally:
            # Clean only this freshly created tree, using the retained fds so
            # Python cleanup cannot itself recurse or depend on PATH_MAX. A
            # cleanup failure cannot mask the primary failure or stop fd release.
            primary_failure=sys.exc_info()[0] is not None
            cleanup_error=None
            try:os.unlink('sentinel',dir_fd=frames[-1])
            except FileNotFoundError:pass
            except OSError as error:cleanup_error=error
            while len(frames)>1:
                child=frames.pop()
                try:os.rmdir('n',dir_fd=frames[-1])
                except FileNotFoundError:pass
                except OSError as error:cleanup_error=cleanup_error or error
                finally:os.close(child)
            os.close(frames.pop())
            try:
                if first_evidence.exists():first_evidence.unlink()
                if victim.exists():victim.rmdir()
            except OSError as error:cleanup_error=cleanup_error or error
            if cleanup_error is not None and not primary_failure:raise cleanup_error

        shallow=self.store/'runs/def-2'
        (shallow/'nested/leaf').mkdir(parents=True)
        (shallow/'nested/leaf/evidence').write_bytes(b'ordinary evidence')
        result=call(self.store,'delete','def-2')
        self.assertEqual(result.returncode,0,result.stderr+result.stdout)
        self.assertFalse(shallow.exists())

    def test_deep_tree_setup_failure_keeps_primary_error_and_closes_every_fd(self):
        original_open,original_close,original_rmdir=os.open,os.close,os.rmdir
        for cleanup_fails in [False,True]:
            with self.subTest(cleanup_fails=cleanup_fails):
                opened=[];closed=[];attempts=0
                def fail_fifth_directory_open(path,flags,*args,**kwargs):
                    nonlocal attempts
                    if flags & os.O_DIRECTORY:
                        attempts+=1
                        if attempts==5:raise OSError(errno.EMFILE,'injected fd exhaustion')
                    descriptor=original_open(path,flags,*args,**kwargs)
                    opened.append(descriptor)
                    return descriptor
                def close_tracked(descriptor):
                    closed.append(descriptor)
                    return original_close(descriptor)
                def fail_cleanup(path,*args,**kwargs):
                    if cleanup_fails:raise OSError(errno.EIO,'injected cleanup failure')
                    return original_rmdir(path,*args,**kwargs)
                with mock.patch.object(os,'open',side_effect=fail_fifth_directory_open), \
                     mock.patch.object(os,'close',side_effect=close_tracked), \
                     mock.patch.object(os,'rmdir',side_effect=fail_cleanup):
                    with self.assertRaises(OSError) as caught:
                        self.test_delete_deep_tree_returns_tool_error_instead_of_stack_abort()
                self.assertEqual(caught.exception.errno,errno.EMFILE)
                self.assertEqual(len(opened),4)
                self.assertEqual(sorted(closed),sorted(opened))
                for descriptor in opened:
                    with self.assertRaises(OSError) as closed_fd:os.fstat(descriptor)
                    self.assertEqual(closed_fd.exception.errno,errno.EBADF)
                victim=self.store/'runs/abc-1'
                if cleanup_fails:shutil.rmtree(victim)
                else:self.assertFalse(victim.exists())

    def test_staging_cleanup_keeps_replaced_inode_alias_on_record_failure(self):
        # Stop the clean owner's actual rename syscall. Replace only the fresh
        # unpublished entry through its directory FD, then return physical EIO.
        faults=FaultBroker(WORK/(self._testMethodName+'-faults'))
        sentinel=WORK/(self._testMethodName+'-sentinel')
        sentinel.write_bytes(b'fresh replacement alias must remain\n')
        name_file=WORK/(self._testMethodName+'-name')
        audit=WORK/(self._testMethodName+'-audit')
        counter=WORK/(self._testMethodName+'-target-once')
        target=WORK/(self._testMethodName+'-target.py')
        target.write_text('import pathlib,sys\nwith pathlib.Path(sys.argv[1]).open("ab") as output: output.write(b"once\\n")\n')
        before={str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()}
        env=dict(ENV,CAPTURE_FAULT_SENTINEL=str(sentinel),CAPTURE_FAULT_NAME=str(name_file),
                 CAPTURE_FAULT_AUDIT=str(audit),PYTHONDONTWRITEBYTECODE='1')
        result=faults.run([str(BIN),'--storage',str(self.store),'record','staging-replacement','--',
                          sys.executable,'-B',str(target),str(counter)],self.store,'stage-replace',environment=env,cwd=WORK)
        LOG.append(dict(args=['record','staging-replacement','events staging replacement'],
                        returncode=result.returncode,stderr=result.stderr.decode(errors='replace')))
        self.assertEqual(result.returncode,2,result.stderr+result.stdout)
        self.assertEqual(result.stdout,b'')
        self.assertIn(b'events.jsonl',result.stderr)
        self.assertIn(b'temporary cleanup failed',result.stderr)
        self.assertIn(b'directory entry no longer names the opened inode',result.stderr)
        self.assertEqual(counter.read_bytes(),b'once\n')
        victim=next(p for p in (self.store/'runs').iterdir() if p!=self.good)
        self.assertFalse((victim/'finalized.json').exists())
        alias_name=name_file.read_text(encoding='ascii')
        self.assertTrue(alias_name.startswith('.rundelta-tmp-') and '/' not in alias_name)
        alias=victim/alias_name
        self.assertTrue(alias.is_file(),'cleanup removed a foreign replacement alias')
        self.assertEqual(alias.read_bytes(),sentinel.read_bytes())
        self.assertEqual((alias.stat().st_dev,alias.stat().st_ino),(sentinel.stat().st_dev,sentinel.stat().st_ino))
        rows=[dict(field.split('=',1) for field in line.split()) for line in audit.read_text().splitlines()]
        self.assertEqual(len(rows),1)
        self.assertEqual((rows[0]['event'],rows[0]['result'],rows[0]['errno']),('rename-failed','-1','5'))
        self.assertEqual(faults.denials(result),1)
        self.assertEqual({str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()},before)
        self.assertEqual(call(self.store,'show','healthy','--json').returncode,0)
        self.assertEqual(call(self.store,'diff','healthy','healthy').returncode,0)

    def test_delete_second_unlink_failure_reports_partial_phase_and_keeps_healthy_record(self):
        compiler=shutil.which('cc')
        self.assertIsNotNone(compiler,'cc required for deterministic delete failure regression')
        source=WORK/(self._testMethodName+'.c')
        library=WORK/(self._testMethodName+'.so')
        source.write_text(r'''
#define _GNU_SOURCE
/* Refuse only the second regular-leaf unlink in this exact fresh record root.
 * The first real unlink succeeds; preflight has already completed. */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
static int (*next_unlinkat)(int, const char *, int);
static pid_t owner;
static dev_t selected_dev;
static ino_t selected_ino;
static unsigned calls;
static const char *audit_output;
__attribute__((constructor)) static void prepare(void) {
    next_unlinkat = dlsym(RTLD_NEXT, "unlinkat");
    if (!next_unlinkat) _exit(96);
    const char *recorder = getenv("RD_DELETE_RECORDER");
    const char *selected = getenv("RD_DELETE_SELECTED");
    audit_output = getenv("RD_DELETE_AUDIT");
    char image[PATH_MAX];
    ssize_t length = readlink("/proc/self/exe", image, sizeof(image) - 1);
    if (length < 0 || !recorder || !selected || !audit_output) return;
    image[length] = '\0';
    if (strcmp(image, recorder)) return;
    int fd = open(selected, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    struct stat status;
    if (fd < 0 || fstat(fd, &status) != 0 || close(fd) != 0) _exit(96);
    selected_dev = status.st_dev;
    selected_ino = status.st_ino;
    owner = getpid();
}
static void audit(unsigned number, const char *name, int result, int saved_errno) {
    char line[512];
    int length = snprintf(line, sizeof(line), "call=%u name=%s result=%d errno=%d\n", number, name, result, saved_errno);
    if (length < 0 || (size_t)length >= sizeof(line)) _exit(96);
    int fd = open(audit_output, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0 || write(fd, line, (size_t)length) != length || close(fd) != 0) _exit(96);
}
int unlinkat(int fd, const char *name, int flags) {
    if (!next_unlinkat) {
        next_unlinkat = dlsym(RTLD_NEXT, "unlinkat");
        if (!next_unlinkat) _exit(96);
    }
    struct stat parent;
    if (owner && getpid() == owner && flags == 0 && fstat(fd, &parent) == 0 &&
        parent.st_dev == selected_dev && parent.st_ino == selected_ino) {
        ++calls;
        if (calls == 2) {
            audit(calls, name, -1, EIO);
            errno = EIO;
            return -1;
        }
        int result = next_unlinkat(fd, name, flags);
        int saved_errno = errno;
        audit(calls, name, result, result == 0 ? 0 : saved_errno);
        errno = saved_errno;
        return result;
    }
    return next_unlinkat(fd, name, flags);
}
''',encoding='ascii')
        compiled=subprocess.run([compiler,'-shared','-fPIC','-Wall','-Wextra','-Werror',
                                 '-o',str(library),str(source),'-ldl'],
                                stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
        self.assertEqual(compiled.returncode,0,compiled.stderr)
        victim=self.synthetic_record('delete-partial-victim')
        healthy_before={str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()}
        victim_before={str(p.relative_to(victim)):p.read_bytes() for p in victim.rglob('*') if p.is_file()}
        audit=WORK/(self._testMethodName+'-audit')
        env=dict(ENV,LD_PRELOAD=str(library),RD_DELETE_RECORDER=str(BIN),
                 RD_DELETE_SELECTED=str(victim),RD_DELETE_AUDIT=str(audit))
        result=subprocess.run([str(BIN),'--storage',str(self.store),'delete',victim.name],env=env,
                              stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
        LOG.append(dict(args=['delete',victim.name,'second unlink fails EIO'],
                        returncode=result.returncode,stderr=result.stderr.decode(errors='replace')))
        self.assertEqual(result.returncode,2,result.stderr+result.stdout)
        self.assertNotIn(b'Deleted',result.stdout)
        self.assertIn(b'removal started; record may be partially deleted',result.stderr)
        self.assertIn(victim.name.encode(),result.stderr)
        self.assertIn(b'Input/output error',result.stderr)
        rows=[dict(part.split('=',1) for part in line.split()) for line in audit.read_text().splitlines()]
        self.assertEqual(len(rows),2,rows)
        self.assertEqual((rows[0]['call'],rows[0]['result'],rows[0]['errno']),('1','0','0'))
        self.assertEqual((rows[1]['call'],rows[1]['result'],rows[1]['errno']),('2','-1',str(errno.EIO)))
        self.assertIn(rows[0]['name'],victim_before)
        self.assertFalse(victim.exists(),'a partial purge must not leave a half-deleted active record')
        listed=call(self.store,'deleted','list','--json')
        self.assertEqual(listed.returncode,0,listed.stderr)
        isolated=json.loads(listed.stdout)
        self.assertEqual(len(isolated),1,isolated)
        self.assertEqual(isolated[0]['id'],victim.name)
        self.assertEqual(isolated[0]['state'],'purge_started')
        ticket=isolated[0]['ticket']
        payload=self.store/'deleted'/ticket/'record'
        self.assertFalse((payload/rows[0]['name']).exists(),'the first actual unlink was not observed')
        self.assertEqual((payload/rows[1]['name']).read_bytes(),victim_before[rows[1]['name']])
        self.assertEqual(call(self.store,'show',victim.name,'--json').returncode,2)
        refused=call(self.store,'deleted','restore',ticket)
        self.assertEqual(refused.returncode,2,refused.stderr)
        self.assertIn(b'cleanup may have started',refused.stderr)
        self.assertEqual({str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()},healthy_before)
        self.assertEqual(call(self.store,'show','healthy','--json').returncode,0)
        self.assertEqual(call(self.store,'diff','healthy','healthy','--json').returncode,0)
        # A new clean delete attempt must take the released recorder/delete lock.
        # This is explicit user-selected recovery, not automatic repair.
        self.assertEqual(call(self.store,'deleted','purge',ticket).returncode,0)
        self.assertFalse(payload.parent.exists())

    def test_staging_cleanup_late_swap_reports_post_unlink_effect(self):
        # The external broker swaps a fresh entry after the owner's final
        # precheck and observes the real unlink and retained inode link count.
        faults=FaultBroker(WORK/(self._testMethodName+'-faults'))
        healthy_before={str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()}
        for mode in ['swap','no-swap','normal']:
            with self.subTest(mode=mode):
                store=self.store/('scenario-'+mode)
                audit=WORK/(self._testMethodName+'-'+mode+'-audit')
                env=dict(ENV,CAPTURE_FAULT_AUDIT=str(audit))
                selected={'swap':'stage-swap','no-swap':'stage-no-swap','normal':'none'}[mode]
                result=faults.run([str(BIN),'--storage',str(store),'record','selected','--','/usr/bin/true'],
                                 store,selected,environment=env,cwd=WORK)
                LOG.append(dict(args=['record','selected','staging-cleanup-mode='+mode],
                                returncode=result.returncode,stderr=result.stderr.decode(errors='replace')))
                record=next((store/'runs').iterdir())
                rows=[dict(part.split('=',1) for part in line.split()) for line in audit.read_text().splitlines()] if audit.exists() else []
                if mode=='normal':
                    self.assertEqual(result.returncode,0,result.stderr+result.stdout)
                    self.assertTrue((record/'finalized.json').is_file())
                    self.assertEqual(rows,[])
                    self.assertNotIn(b'temporary cleanup failed',result.stderr)
                else:
                    self.assertEqual(result.returncode,2,result.stderr+result.stdout)
                    self.assertEqual(result.stdout,b'')
                    self.assertFalse((record/'finalized.json').exists())
                    self.assertIn(b'before publish during rename temporary file',result.stderr)
                    self.assertEqual(len(rows),2,rows)
                    self.assertEqual((rows[0]['event'],rows[0]['result'],rows[0]['errno']),('rename-failed','-1','5'))
                    self.assertEqual((rows[1]['event'],rows[1]['result'],rows[1]['errno']),('cleanup-unlink','0','0'))
                    self.assertFalse((record/rows[1]['temporary']).exists(),'unlink really removed this name')
                    if mode=='swap':
                        self.assertEqual(rows[1]['links'],'1')
                        self.assertTrue((record/rows[1]['retained']).is_file(),'owned inode still has a name')
                        self.assertIn(b'temporary cleanup failed',result.stderr)
                        self.assertIn(b'owned temporary inode remains linked after removal',result.stderr)
                    else:
                        self.assertEqual(rows[1]['links'],'0')
                        self.assertNotIn(b'temporary cleanup failed',result.stderr)
                self.assertEqual({str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()},healthy_before)

    def test_late_delete_swap_cannot_report_selected_inode_removed(self):
        # Place one controlled swap after the final identity precheck. All
        # paths are fresh test records; the interposer never selects a process.
        compiler=shutil.which('cc')
        self.assertIsNotNone(compiler,'cc required for deterministic delete-race regression')
        source=WORK/(self._testMethodName+'.c')
        library=WORK/(self._testMethodName+'.so')
        source.write_text(r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
int unlinkat(int fd, const char *name, int flags) {
    int (*original)(int, const char *, int) = dlsym(RTLD_NEXT, "unlinkat");
    const char *selected = getenv("RUNDELTA_TEST_SWAP_LEAF");
    if (!original) { errno = EIO; return -1; }
    if (selected && flags == AT_REMOVEDIR && strcmp(name, selected) == 0) {
        if (renameat(fd, name, fd, "opened-record") != 0 ||
            mkdirat(fd, name, 0700) != 0) return -1;
    }
    return original(fd, name, flags);
}
''')
        compiled=subprocess.run([compiler,'-shared','-fPIC','-Wall','-Wextra','-Werror',
                                 '-o',str(library),str(source),'-ldl'],
                                stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
        self.assertEqual(compiled.returncode,0,compiled.stderr)
        for nested in [False,True]:
            with self.subTest(nested=nested):
                victim=self.store/'runs/abc-1'
                victim.mkdir()
                selected=victim/'selected-child' if nested else victim
                if nested:selected.mkdir()
                env=dict(ENV,LD_PRELOAD=str(library),RUNDELTA_TEST_SWAP_LEAF=selected.name if nested else 'record')
                result=subprocess.run([str(BIN),'--storage',str(self.store),'delete','abc-1'],
                                      env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
                LOG.append(dict(args=['delete','abc-1',f'late-swap-nested={nested}'],
                                returncode=result.returncode,stderr=result.stderr.decode(errors='replace')))
                slot=next((self.store/'deleted').iterdir())
                retained=(slot/'record' if nested else slot)/'opened-record'
                self.assertTrue(retained.is_dir())
                self.assertEqual(result.returncode,2,result.stderr+result.stdout)
                self.assertIn(b'selected directory remains linked after removal',result.stderr)
                self.assertNotIn(b'Deleted',result.stdout)
                self.assertEqual(call(self.store,'show','healthy','--json').returncode,0)
                shutil.rmtree(slot) # test-owned failed slot; never real user storage
                self.assertIn(b'removal started; record may be partially deleted',result.stderr)

        victim=self.store/'runs/def-2'
        (victim/'ordinary').mkdir(parents=True)
        env=dict(ENV,LD_PRELOAD=str(library))
        result=subprocess.run([str(BIN),'--storage',str(self.store),'delete','def-2'],
                              env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
        self.assertEqual(result.returncode,0,result.stderr+result.stdout)
        self.assertFalse(victim.exists())

    def test_deleted_protocol_isolates_crash_and_failure_before_explicit_recovery(self):
        compiler=shutil.which('cc')
        self.assertIsNotNone(compiler)
        source=ROOT/'tests/probes/deletion_protocol.c'
        library=WORK/(self._testMethodName+'.so')
        built=subprocess.run([compiler,'-shared','-fPIC','-Wall','-Wextra','-Werror',
                              '-o',str(library),str(source),'-ldl'],capture_output=True,timeout=15)
        self.assertEqual(built.returncode,0,built.stderr)
        healthy_before={str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()}
        for mode in ['isolation-fail','commit-interrupt','intent-sync-fail','purge-first-fail','restore-fail']:
            with self.subTest(mode=mode):
                victim=self.synthetic_record('isolation-'+mode)
                before={str(p.relative_to(victim)):p.read_bytes() for p in victim.rglob('*') if p.is_file()}
                audit=WORK/(self._testMethodName+'-'+mode+'.audit')
                injected='commit-interrupt' if mode=='restore-fail' else mode
                environment=dict(ENV,LD_PRELOAD=str(library),RD_DP_BINARY=str(BIN),RD_DP_MODE=injected,RD_DP_AUDIT=str(audit))
                result=subprocess.run([str(BIN),'--storage',str(self.store),'delete',victim.name],
                                      env=environment,capture_output=True,timeout=15)
                self.assertEqual(result.returncode,87 if injected=='commit-interrupt' else 2,result.stderr)
                listed=call(self.store,'deleted','list','--json')
                self.assertEqual(listed.returncode,0,listed.stderr)
                rows=json.loads(listed.stdout)
                self.assertEqual(len(rows),1,rows)
                row=rows[0];ticket=row['ticket'];payload=self.store/'deleted'/ticket/'record'
                if mode=='isolation-fail':
                    self.assertEqual(row['state'],'no_record')
                    self.assertEqual({str(p.relative_to(victim)):p.read_bytes() for p in victim.rglob('*') if p.is_file()},before)
                    self.assertEqual(call(self.store,'deleted','restore',ticket).returncode,2)
                    self.assertEqual(call(self.store,'deleted','purge',ticket).returncode,0)
                else:
                    self.assertFalse(victim.exists())
                    self.assertEqual({str(p.relative_to(payload)):p.read_bytes() for p in payload.rglob('*') if p.is_file()},before)
                    if mode in ['intent-sync-fail','purge-first-fail']:
                        self.assertEqual(row['state'],'purge_started')
                        self.assertEqual(call(self.store,'deleted','restore',ticket).returncode,2)
                        self.assertEqual(call(self.store,'deleted','purge',ticket).returncode,0)
                    else:
                        self.assertEqual(row['state'],'isolated')
                        if mode=='restore-fail':
                            environment['RD_DP_MODE']='restore-fail'
                            failed=subprocess.run([str(BIN),'--storage',str(self.store),'deleted','restore',ticket],
                                                  env=environment,capture_output=True,timeout=15)
                            self.assertEqual(failed.returncode,2,failed.stderr)
                            self.assertFalse(victim.exists())
                        self.assertEqual(call(self.store,'deleted','restore',ticket).returncode,0)
                        self.assertEqual({str(p.relative_to(victim)):p.read_bytes() for p in victim.rglob('*') if p.is_file()},before)
                if victim.exists(): self.assertEqual(call(self.store,'delete',victim.name).returncode,0)
                self.assertEqual({str(p.relative_to(self.good)):p.read_bytes() for p in self.good.rglob('*') if p.is_file()},healthy_before)

    def test_closed_pipe_keeps_diff_status(self):
        d=self.damage('no_marker')
        for suffix in [(),('--json',)]:
            r,w=os.pipe();os.close(r)
            try:p=call(self.store,'diff',d.name,'healthy',*suffix,stdout=w)
            finally:os.close(w)
            self.assertEqual(p.returncode,3,p.stderr)
        (d/'metadata.json').write_text('{broken')
        r,w=os.pipe();os.close(r)
        try:p=call(self.store,'diff',d.name,'healthy','--json',stdout=w)
        finally:os.close(w)
        self.assertEqual(p.returncode,2,p.stderr)
        self.assertNotIn(b'panicked',p.stderr)

    def test_diff_json_output_failures_preserve_versions_statuses_and_inputs(self):
        """Sink errors preserve comparison facts and never repair saved inputs.

        Fresh legacy/default and trace-v1/explicit views cover all four statuses.
        A large metadata change crosses the JSON buffer without altering raw or
        normalized evidence; existing v6 tests retain the full-reference oracle.
        """
        artifact=WORK/(self._testMethodName+'-evidence')
        artifact.mkdir()
        legacy=dict(base=self.good)
        trace=dict(base=self.trace_v1_copy('output-trace-base'))
        for family,controls in [('legacy',legacy),('trace',trace)]:
            for state in ['changed','corrupt','unfinalized']:
                label='output-'+family+'-'+state
                directory=self.synthetic_record(label) if family=='legacy' else self.trace_v1_copy(label)
                controls[state]=directory
                if state=='changed':
                    metadata=json.loads((directory/'metadata.json').read_bytes())
                    metadata['command'].append('--output-small-control')
                    (directory/'metadata.json').write_text(json.dumps(metadata))
                    if family=='legacy':self.refresh_marker(directory)
                    else:self.refresh_trace_v1_marker(directory)
                elif state=='corrupt':(directory/'metadata.json').write_bytes(b'{broken')
                else:(directory/'finalized.json').unlink()
        large=self.trace_v1_copy('output-trace-large')
        metadata=json.loads((large/'metadata.json').read_bytes())
        metadata['command'].append('--output-large-control-'+'x'*12288)
        (large/'metadata.json').write_text(json.dumps(metadata))
        self.refresh_trace_v1_marker(large)
        for directory in [trace['base'],trace['changed'],large]:
            self.assert_trace_v1_wire(directory,self.mapping_run(directory))

        cases=[]
        for family,controls,versions in [
                ('legacy',legacy,[('default',(),4)]),
                ('trace',trace,[('default',(),5),('v5',('--report-version','5'),5),
                                ('v6',('--report-version','6'),6)])]:
            for version,arguments,schema in versions:
                for status,left,right in [(0,controls['base'],controls['base']),
                                          (1,controls['base'],controls['changed']),
                                          (2,controls['corrupt'],controls['base']),
                                          (3,controls['unfinalized'],controls['unfinalized'])]:
                    cases.append((family+'-'+version+'-'+str(status),left,right,status,arguments,schema,False))
        for version,arguments,schema in [('default',(),5),('v5',('--report-version','5'),5),
                                         ('v6',('--report-version','6'),6)]:
            cases.append(('large-'+version,trace['base'],large,1,arguments,schema,True))

        before={str(path.relative_to(self.store)):path.read_bytes()
                for path in self.store.rglob('*') if path.is_file()}
        for relative,payload in before.items():
            saved=artifact/'before'/relative
            saved.parent.mkdir(parents=True,exist_ok=True)
            saved.write_bytes(payload)
        sink_commands=[]
        try:
            for label,left,right,status,version,schema,is_large in cases:
                with self.subTest(case=label):
                    arguments=('diff',left.name,right.name,*version,'--json')
                    normal=self.mapping_call(*arguments)
                    self.assertEqual(normal.returncode,status,normal.stderr+normal.stdout)
                    self.assertEqual(normal.stderr,b'')
                    self.assertTrue(normal.stdout.endswith(b'\n'))
                    self.assertFalse(normal.stdout.endswith(b'\n\n'))
                    report=json.loads(normal.stdout)
                    self.assertEqual(report['schema_version'],schema)
                    self.assertEqual(report['exit_code'],status)
                    if status==2:
                        self.assertIsNone(report['observed_differences'])
                        self.assertEqual(report['changes'],[])
                        self.assertTrue(report['errors'])
                    else:self.assertEqual(report['observed_differences'],status==1)
                    if status==3:
                        for side in ['left','right']:
                            self.assertEqual(report['reliability'][side]['storage'],'unfinalized')
                    if schema==6:
                        self.assertEqual(report['evidence_contract'],'outcome_event_references_v1')
                        if status in [0,1]:
                            self.assertEqual(report['target_provenance'],{
                                side:json.loads((directory/'metadata.json').read_bytes())['target_provenance']
                                for side,directory in [('left',left),('right',right)]})
                        if status==1:
                            self.assertEqual(len(report['changes']),1)
                            change=report['changes'][0]
                            self.assertEqual(change['category'],'Command arguments')
                            for side in ['left','right']:
                                self.assert_v6_metadata_side(change,side,['metadata.command'])
                    if is_large:self.assertGreater(len(normal.stdout),8192)

                    read_fd,write_fd=os.pipe()
                    os.close(read_fd)
                    try:closed=call(self.store,*arguments,stdout=write_fd)
                    finally:os.close(write_fd)
                    self.assertEqual(closed.returncode,status,closed.stderr)
                    self.assertEqual(closed.stderr,b'')
                    with open('/dev/full','wb') as sink:
                        full=call(self.store,*arguments,stdout=sink)
                    self.assertEqual(full.returncode,2,full.stderr)
                    self.assertIn(b'stdout',full.stderr)
                    self.assertNotIn(b'corrupt',full.stderr)
                    self.assertNotIn(b'panicked',full.stderr)
                    for destination,result in [('closed-pipe',closed),('dev-full',full)]:
                        stderr_path=artifact/(label+'-'+destination+'.stderr')
                        stderr_path.write_bytes(result.stderr)
                        sink_commands.append(dict(args=arguments,destination=destination,
                            returncode=result.returncode,stderr_file=str(stderr_path),
                            stderr_sha256=hashlib.sha256(result.stderr).hexdigest()))
                    replay=self.mapping_call(*arguments)
                    self.assertEqual(replay.returncode,status,replay.stderr+replay.stdout)
                    self.assertEqual(replay.stderr,normal.stderr)
                    self.assertEqual(replay.stdout,normal.stdout)
        finally:
            after={str(path.relative_to(self.store)):path.read_bytes()
                   for path in self.store.rglob('*') if path.is_file()}
            for relative,payload in after.items():
                saved=artifact/'after'/relative
                saved.parent.mkdir(parents=True,exist_ok=True)
                saved.write_bytes(payload)
            (artifact/'inputs.json').write_text(json.dumps({phase:{relative:dict(bytes=len(payload),
                sha256=hashlib.sha256(payload).hexdigest()) for relative,payload in snapshot.items()}
                for phase,snapshot in [('before',before),('after',after)]},indent=2))
            (artifact/'sink-commands.json').write_text(json.dumps(sink_commands,indent=2))
        self.assertEqual(before,after)

    def test_index_hints_and_nonregular_files(self):
        d=self.damage('checksum')
        m=json.loads((d/'metadata.json').read_text())
        for mutation in ['future_version','wrong_id','missing_field']:
            modified=dict(m)
            if mutation=='future_version':
                modified['schema_version']=999
                modified.pop('command')
            elif mutation=='wrong_id':modified['id']='bad-999'
            else:modified.pop('command')
            (d/'metadata.json').write_text(json.dumps(modified))
            self.assertEqual(call(self.store,'show','healthy').returncode,0)
            self.assertEqual(call(self.store,'show','damaged').returncode,2)
            self.assertEqual(call(self.store,'record','damaged','--','/usr/bin/true').returncode,2)
            listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
            row=next(r for r in json.loads(listing.stdout) if r['id']==d.name)
            # This fixture already has a stale finalization checksum. Finalized
            # bytes are authenticated before their schema/DTO is interpreted,
            # so even a speculative future header remains corrupt here. The
            # verified-future-header case below separately freezes unsupported.
            self.assertEqual(row['storage_integrity']['status'],'corrupt')
            self.assertIn('byte count or SHA-256 mismatch',row['storage_integrity']['detail'])
            if mutation=='future_version':
                compared=call(self.store,'diff','damaged','healthy','--json')
                self.assertEqual(compared.returncode,2,compared.stderr+compared.stdout)
                report=json.loads(compared.stdout)
                self.assertIsNone(report['observed_differences'])
                self.assertEqual(report['reliability']['left']['storage'],'corrupt')
        (d/'metadata.json').unlink()
        (d/'metadata.json').symlink_to(self.good/'metadata.json')
        self.assertEqual(call(self.store,'show',d.name).returncode,2)
        self.assertEqual(call(self.store,'show','healthy').returncode,0)
        listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
        row=next(r for r in json.loads(listing.stdout) if r['id']==d.name)
        self.assertEqual(row['storage_integrity']['status'],'unavailable')
        (d/'metadata.json').unlink();os.mkfifo(d/'metadata.json')
        self.assertEqual(call(self.store,'show',d.name).returncode,2)
        listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
        row=next(r for r in json.loads(listing.stdout) if r['id']==d.name)
        self.assertEqual(row['storage_integrity']['status'],'unavailable')
        self.assertEqual(call(self.store,'delete',d.name).returncode,0)
        self.assertTrue(self.good.exists())

    def test_future_event_and_marker_headers_are_unsupported(self):
        events=self.good/'events.jsonl'
        marker_path=self.good/'finalized.json'
        original_events=events.read_bytes()
        original_marker=marker_path.read_bytes()

        future_event=json.loads(original_events.splitlines()[0])
        future_event['schema_version']=999
        future_event.pop('pid')
        future_bytes=(json.dumps(future_event,separators=(',',':'))+'\n').encode()
        events.write_bytes(future_bytes)
        marker=json.loads(original_marker)
        marker['events']={'bytes':len(future_bytes),'sha256':hashlib.sha256(future_bytes).hexdigest()}
        marker_path.write_text(json.dumps(marker))
        listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
        row=next(r for r in json.loads(listing.stdout) if r['id']==self.good.name)
        self.assertEqual(row['storage_integrity']['status'],'unsupported')
        compared=call(self.store,'diff','healthy','healthy','--json')
        self.assertEqual(compared.returncode,2,compared.stderr+compared.stdout)
        report=json.loads(compared.stdout)
        self.assertEqual(report['reliability']['left']['storage'],'unavailable')
        self.assertIsNone(report['observed_differences'])

        events.write_bytes(original_events)
        marker_path.write_text(json.dumps({'schema_version':999}))
        listing=call(self.store,'list','--json');self.assertEqual(listing.returncode,0,listing.stderr)
        row=next(r for r in json.loads(listing.stdout) if r['id']==self.good.name)
        self.assertEqual(row['storage_integrity']['status'],'unsupported')
        shown=call(self.store,'show','healthy','--json')
        self.assertEqual(shown.returncode,2,shown.stderr+shown.stdout)
        self.assertIn(b'unsupported finalization marker schema',shown.stderr)

        marker_path.write_bytes(original_marker)

    def test_legacy_next_to_damage(self):
        d=self.damage('bad_metadata')
        marker=self.good/'finalized.json';marker.unlink()
        meta=self.good/'metadata.json';m=json.loads(meta.read_text())
        for version in [2,1]:
            m['schema_version']=version
            if version==1:
                for key in ['environment','completeness']:m.pop(key,None)
            meta.write_text(json.dumps(m))
            p=call(self.store,'show','healthy','--json');self.assertEqual(p.returncode,0,p.stderr)
            self.assertEqual(json.loads(p.stdout)['storage_integrity']['status'],'unverified')
            self.assertEqual(call(self.store,'diff','healthy','healthy').returncode,3)
        self.assertTrue(d.exists())

    def test_trace_v1_capture_diff_and_legacy_versions(self):
        left,left_run=self.record_trace_v1('trace-left')
        right,right_run=self.record_trace_v1('trace-right')
        changed,changed_run=self.record_trace_v1('trace-changed',('/usr/bin/false',),expected=1)
        for directory,run in [(left,left_run),(right,right_run),(changed,changed_run)]:
            self.assert_trace_v1_wire(directory,run)
        equal=self.trace_v5_report('trace-left','trace-right',0)
        self.assertFalse(equal['observed_differences'],equal)
        self.assertEqual(equal['changes'],[],equal)
        for side in ['left','right']:
            self.assertEqual(equal['reliability'][side],{
                'capture':'complete','storage':'verified','raw':'verified','provenance':'verified',
            },equal)
        self.assertEqual(equal['target_provenance'],{
            'left':left_run['metadata']['target_provenance'],
            'right':right_run['metadata']['target_provenance'],
        })
        different=self.trace_v5_report('trace-left','trace-changed',1)
        self.assertTrue(different['observed_differences'],different)
        self.assertTrue(different['changes'],different)
        self.assertEqual(different['target_provenance']['right'],changed_run['metadata']['target_provenance'])

        # Opt-in generation fields and raw commitments cannot leak into the old
        # record/display contract, even after new records coexist in its store.
        legacy=json.loads(call(self.store,'show','healthy','--json').stdout)
        self.assertEqual(legacy['metadata']['schema_version'],3,legacy)
        self.assertNotIn('target_provenance',legacy['metadata'])
        self.assertNotIn('raw_integrity',legacy)
        self.assertTrue(all(event['schema_version']==2 for event in legacy['events']))
        self.assertTrue(all('task_generation' not in event and 'child_generation' not in event
                            for event in legacy['events']))
        self.assertEqual(json.loads((self.good/'finalized.json').read_bytes())['schema_version'],1)
        old_report=call(self.store,'diff','healthy','healthy','--json')
        self.assertEqual(old_report.returncode,0,old_report.stderr+old_report.stdout)
        old_report=json.loads(old_report.stdout)
        self.assertEqual(old_report['schema_version'],4,old_report)
        self.assertNotIn('raw',old_report['reliability']['left'])
        self.assertNotIn('provenance',old_report['reliability']['left'])
        self.assertNotIn('target_provenance',old_report)

    def test_trace_v5_old_and_mixed_inputs_remain_unverified(self):
        _,new=self.record_trace_v1('trace-new')
        old=self.trace_v5_report('healthy','healthy',3,explicit=True)
        self.assertFalse(old['observed_differences'],old)
        self.assertEqual(old['target_provenance'],{'left':None,'right':None})
        for side in ['left','right']:
            self.assertEqual(old['reliability'][side]['storage'],'verified',old)
            self.assertEqual(old['reliability'][side]['raw'],'unverified',old)
            self.assertEqual(old['reliability'][side]['provenance'],'unverified',old)
        for left,right,new_side,old_side in [
            ('trace-new','healthy','left','right'),('healthy','trace-new','right','left'),
        ]:
            with self.subTest(new_side=new_side):
                mixed=self.trace_v5_report(left,right,3)
                self.assertEqual(mixed['reliability'][new_side]['raw'],'verified',mixed)
                self.assertEqual(mixed['reliability'][new_side]['provenance'],'verified',mixed)
                self.assertEqual(mixed['reliability'][old_side]['storage'],'verified',mixed)
                self.assertEqual(mixed['reliability'][old_side]['raw'],'unverified',mixed)
                self.assertEqual(mixed['reliability'][old_side]['provenance'],'unverified',mixed)
                self.assertEqual(mixed['target_provenance'][new_side],new['metadata']['target_provenance'])
                self.assertIsNone(mixed['target_provenance'][old_side])

    def test_trace_v1_spawn_generation_keeps_child_exit_separate(self):
        target='import os\npid=os.fork()\nif pid==0: os._exit(9)\nos.waitpid(pid,0)\nos._exit(0)\n'
        directory,run=self.record_trace_v1('trace-spawn',(sys.executable,'-c',target))
        self.assert_trace_v1_wire(directory,run)
        spawn=[event for event in run['events'] if event['kind']=='spawn' and event['child_pid'] is not None]
        self.assertTrue(spawn,run['events'])
        provenance=run['metadata']['target_provenance']
        self.assertEqual(run['metadata']['exit_code'],0,run['metadata'])
        for event in spawn:
            children=[child for child in run['events'] if child['pid']==event['child_pid']]
            self.assertTrue(children,event)
            self.assertTrue(all(child['task_generation']==event['child_generation'] for child in children),children)
            child_exits=[child for child in children if child['kind']=='exit']
            self.assertTrue(child_exits,children)
            self.assertTrue(any(child['outcome']=='+++ exited with 9 +++' for child in child_exits),child_exits)
            self.assertNotEqual(event['child_pid'],provenance['root']['pid'])
        self.trace_v5_report(directory.name,directory.name,0)

    def test_trace_v5_raw_failures_preserve_verified_structured_facts(self):
        for damage in ['same-length','truncated','grown','missing','unreadable','symlink','fifo']:
            with self.subTest(damage=damage):
                directory=self.trace_v1_copy('trace-raw-'+damage)
                trace=directory/'raw/trace.log'
                original=trace.read_bytes()
                structured={name:(directory/name).read_bytes() for name in [
                    'metadata.json','events.jsonl','raw-integrity.json','finalized.json',
                ]}
                if damage=='same-length':trace.write_bytes(bytes([original[0]^1])+original[1:])
                elif damage=='truncated':trace.write_bytes(original[:-1])
                elif damage=='grown':trace.write_bytes(original+b' \n')
                elif damage=='missing':trace.unlink()
                elif damage=='unreadable':
                    self.assertNotEqual(os.geteuid(),0,'permission regression requires non-root')
                    trace.chmod(0)
                elif damage=='symlink':
                    trace.unlink();trace.symlink_to(self.trace_template/'raw/trace.log')
                else:trace.unlink();os.mkfifo(trace)
                expected='unavailable' if damage in ['unreadable','symlink','fifo'] else 'corrupt'
                try:
                    report=self.trace_v5_report(directory.name,'trace-template',2)
                    self.assertIsNone(report['observed_differences'],report)
                    self.assertEqual(report['changes'],[],report)
                    self.assertEqual(report['reliability']['left']['storage'],'verified',report)
                    self.assertEqual(report['reliability']['left']['raw'],expected,report)
                    self.assertEqual(report['reliability']['right']['storage'],'verified',report)
                    self.assertEqual(report['reliability']['right']['raw'],'verified',report)
                    self.assertTrue(report['errors'],report)
                    # Reads never repair raw files, refresh hashes, or manufacture
                    # a replacement marker that could hide the negative control.
                    self.assertEqual(structured,{name:(directory/name).read_bytes() for name in structured})
                    if damage=='missing':self.assertFalse(trace.exists())
                    elif damage=='same-length':self.assertEqual(len(trace.read_bytes()),len(original))
                    elif damage=='symlink':self.assertTrue(trace.is_symlink())
                finally:
                    if damage=='unreadable':trace.chmod(0o600)

    def test_trace_v5_explicit_version_survives_two_structured_failures(self):
        directories=[self.trace_v1_copy('trace-broken-'+str(index)) for index in range(2)]
        for directory in directories:(directory/'metadata.json').write_bytes(b'{broken')
        report=self.trace_v5_report(directories[0].name,directories[1].name,2,explicit=True)
        self.assertIsNone(report['observed_differences'],report)
        self.assertEqual(report['changes'],[],report)
        self.assertGreaterEqual(len(report['errors']),2,report)

    def test_trace_v1_saved_generation_selects_the_exact_reused_pid_exit(self):
        complete=self.synthetic_trace_v1('trace-synthetic-complete')
        shown=call(self.store,'show',complete.name,'--json')
        self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
        self.assert_trace_v1_wire(complete,json.loads(shown.stdout))
        self.trace_v5_report(complete.name,complete.name,0)

        reused=self.synthetic_trace_v1('trace-reused-pid',reused_pid=True)
        shown=call(self.store,'show',reused.name,'--json')
        self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
        run=json.loads(shown.stdout)
        self.assert_trace_v1_wire(reused,run)
        self.assertEqual(run['events'][0]['outcome'],run['events'][2]['outcome'])
        self.assertEqual(run['events'][0]['pid'],run['events'][2]['pid'])
        self.assertEqual(run['metadata']['target_provenance']['root']['generation'],2)
        self.assertEqual(run['metadata']['target_provenance']['target_exit']['index'],2)
        report=self.trace_v5_report(reused.name,reused.name,3)
        self.assertEqual(report['reliability']['left']['raw'],'verified',report)
        self.assertEqual(report['reliability']['left']['provenance'],'verified',report)
        self.assertEqual(report['target_provenance']['left']['target_exit']['evidence']['line'],3)

    def test_trace_v1_matching_hashes_do_not_justify_invalid_provenance(self):
        mutations=['wrong-exit-generation','wrong-root-generation','wrong-root-pid',
                   'exit-index-overflow','root-exec-is-exit','exit-is-exec','wrong-exit-coordinate',
                   'later-successful-root-exec','closed-generation-reappears',
                   'erase-observed-target','identity-promotion',
                   'unknown-selection-rule','unknown-identity-contract','missing-provenance',
                   'zero-event-generation','missing-event-generation','child-generation-on-exit']
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                directory=self.synthetic_trace_v1('trace-provenance-'+mutation,reused_pid=True)
                metadata=json.loads((directory/'metadata.json').read_bytes())
                events=[json.loads(line) for line in (directory/'events.jsonl').read_bytes().splitlines()]
                provenance=metadata['target_provenance']
                if mutation=='wrong-exit-generation':
                    provenance['target_exit']=dict(index=0,evidence=events[0]['evidence'])
                elif mutation=='wrong-root-generation':provenance['root']['generation']=1
                elif mutation=='wrong-root-pid':provenance['root']['pid']='42'
                elif mutation=='exit-index-overflow':provenance['target_exit']['index']=len(events)
                elif mutation=='root-exec-is-exit':
                    provenance['root_exec']=dict(index=2,evidence=events[2]['evidence'])
                elif mutation=='exit-is-exec':
                    provenance['target_exit']=dict(index=1,evidence=events[1]['evidence'])
                elif mutation=='wrong-exit-coordinate':provenance['target_exit']['evidence']['line']=2
                elif mutation in ['later-successful-root-exec','closed-generation-reappears']:
                    later=json.loads(json.dumps(events[1]))
                    if mutation=='closed-generation-reappears':
                        # Root references remain valid; an unrelated file event
                        # must not borrow the earlier, already exited lifetime.
                        later.update(kind='file',operation='openat',path='/baseline/input.txt',
                                     argv=None,task_generation=1)
                    later['evidence'].update(line=3,end_line=3)
                    events.insert(2,later)
                    events[3]['evidence'].update(line=4,end_line=4)
                    if mutation=='later-successful-root-exec':
                        # Both execs are successful in one lifetime. Only its
                        # first entry satisfies the frozen target selection.
                        provenance['root_exec']=dict(index=2,evidence=later['evidence'])
                    provenance['target_exit']=dict(index=3,evidence=events[3]['evidence'])
                    metadata.update(parsed_events=4,raw_lines=4)
                    raw=(directory/'raw/trace.log').read_bytes().splitlines()
                    raw.insert(2,raw[1] if mutation=='later-successful-root-exec' else
                               b'41 openat(AT_FDCWD, "/baseline/input.txt", O_RDONLY) = 3</baseline/input.txt>')
                    raw=b'\n'.join(raw)+b'\n'
                    (directory/'raw/trace.log').write_bytes(raw)
                    manifest=json.loads((directory/'raw-integrity.json').read_bytes())
                    manifest['receipt'].update(bytes=len(raw),logical_lines=4,sha256=hashlib.sha256(raw).hexdigest())
                    (directory/'raw-integrity.json').write_text(json.dumps(manifest))
                elif mutation=='erase-observed-target':
                    # Failed supervision cannot erase the root exec/exit facts
                    # that this same finalized event stream still records.
                    metadata.update(state='capture_failed',completeness='incomplete',exit_code=None,signal=None,
                                    target_exit_raw=None,target_provenance=None)
                elif mutation=='identity-promotion':
                    # Reused task IDs remain uncertain even when all checksums
                    # are recomputed around a falsely complete declaration.
                    for event in events:event['context_uncertain']=False
                    metadata.update(warnings=[],completeness='complete_for_supported_events')
                elif mutation=='unknown-selection-rule':provenance['selection_rule']='future_rule'
                elif mutation=='unknown-identity-contract':provenance['identity_contract']='future_identity'
                elif mutation=='missing-provenance':metadata['target_provenance']=None
                elif mutation=='zero-event-generation':events[2]['task_generation']=0
                elif mutation=='missing-event-generation':events[2].pop('task_generation')
                else:events[2]['child_generation']=1
                (directory/'metadata.json').write_text(json.dumps(metadata))
                (directory/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
                self.refresh_trace_v1_marker(directory)
                shown=call(self.store,'show',directory.name,'--json')
                self.assertEqual(shown.returncode,2,shown.stderr+shown.stdout)
                report=self.trace_v5_report(directory.name,'trace-template',2)
                self.assertIsNone(report['observed_differences'],report)
                self.assertEqual(report['changes'],[],report)
                self.assertEqual(report['reliability']['right']['storage'],'verified',report)
                self.assertEqual(report['reliability']['right']['raw'],'verified',report)

    def test_trace_v1_manifest_and_tuple_controls_fail_closed(self):
        for mutation in ['missing-manifest','bad-manifest','wrong-run-id','wrong-file',
                         'false-byte-count','false-line-count','self-consistent-false-line-count',
                         'false-sha256','future-manifest',
                         'marker-v1','event-v2','metadata-v3']:
            with self.subTest(mutation=mutation):
                directory=self.synthetic_trace_v1('trace-manifest-'+mutation)
                path=directory/'raw-integrity.json'
                manifest=json.loads(path.read_bytes())
                if mutation=='missing-manifest':path.unlink()
                elif mutation=='bad-manifest':path.write_bytes(b'{broken');self.refresh_trace_v1_marker(directory)
                elif mutation=='marker-v1':
                    marker=json.loads((directory/'finalized.json').read_bytes())
                    marker['schema_version']=1;marker.pop('raw_manifest')
                    (directory/'finalized.json').write_text(json.dumps(marker))
                elif mutation=='event-v2':
                    events=[json.loads(line) for line in (directory/'events.jsonl').read_bytes().splitlines()]
                    events[0]['schema_version']=2
                    (directory/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
                    self.refresh_trace_v1_marker(directory)
                elif mutation=='metadata-v3':
                    metadata=json.loads((directory/'metadata.json').read_bytes())
                    metadata['schema_version']=3
                    (directory/'metadata.json').write_text(json.dumps(metadata))
                    self.refresh_trace_v1_marker(directory)
                else:
                    if mutation=='wrong-run-id':manifest['run_id']='abc999-999'
                    elif mutation=='wrong-file':manifest['file']='../trace.log'
                    elif mutation=='false-byte-count':manifest['receipt']['bytes']+=1
                    elif mutation=='false-line-count':manifest['receipt']['logical_lines']+=1
                    elif mutation=='self-consistent-false-line-count':
                        # Agreement between two saved fields is insufficient:
                        # the receipt must also match the measured raw framing.
                        manifest['receipt']['logical_lines']+=1
                        metadata=json.loads((directory/'metadata.json').read_bytes())
                        metadata['raw_lines']+=1
                        (directory/'metadata.json').write_text(json.dumps(metadata))
                    elif mutation=='false-sha256':manifest['receipt']['sha256']='0'*64
                    else:manifest['schema_version']=999
                    path.write_text(json.dumps(manifest));self.refresh_trace_v1_marker(directory)
                before={name:(directory/name).read_bytes() for name in [
                    'metadata.json','events.jsonl','finalized.json',
                ]}
                report=self.trace_v5_report(directory.name,'trace-template',2)
                self.assertIsNone(report['observed_differences'],report)
                self.assertEqual(report['changes'],[],report)
                self.assertEqual(report['reliability']['right']['raw'],'verified',report)
                if mutation=='self-consistent-false-line-count':
                    self.assertEqual(report['reliability']['left']['storage'],'verified',report)
                    self.assertEqual(report['reliability']['left']['raw'],'corrupt',report)
                self.assertEqual(before,{name:(directory/name).read_bytes() for name in before})
                if mutation=='missing-manifest':self.assertFalse(path.exists())

    def test_trace_v1_unverified_receipt_and_unfinalized_record_stay_incomplete(self):
        directory=self.trace_v1_copy('trace-unverified-input')
        metadata=json.loads((directory/'metadata.json').read_bytes())
        metadata.update(state='capture_failed',completeness='incomplete',exit_code=None,signal=None,
                        target_exit_raw=None,target_provenance=None,parsed_events=0,raw_lines=0,
                        warnings=['synthetic normalization input was deliberately skipped'])
        (directory/'metadata.json').write_text(json.dumps(metadata))
        (directory/'events.jsonl').write_bytes(b'')
        manifest=json.loads((directory/'raw-integrity.json').read_bytes())
        manifest['receipt']={'status':'unverified','reason':'synthetic normalization input was deliberately skipped'}
        (directory/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(directory)
        report=self.trace_v5_report(directory.name,directory.name,3)
        self.assertFalse(report['observed_differences'],report)
        self.assertEqual(report['reliability']['left']['storage'],'verified',report)
        self.assertEqual(report['reliability']['left']['raw'],'unverified',report)
        self.assertEqual(report['reliability']['left']['provenance'],'missing',report)
        self.assertEqual(report['target_provenance'],{'left':None,'right':None})
        # No checksum was promised here: later raw changes cannot invent a
        # stronger receipt, a target result, or an ordinary trusted comparison.
        (directory/'raw/trace.log').write_bytes(b'different unverified raw bytes\n')
        self.trace_v5_report(directory.name,directory.name,3)

        unfinalized=self.trace_v1_copy('trace-no-final-marker')
        (unfinalized/'finalized.json').unlink()
        before={str(path.relative_to(unfinalized)):path.read_bytes()
                for path in unfinalized.rglob('*') if path.is_file()}
        report=self.trace_v5_report(unfinalized.name,unfinalized.name,3)
        self.assertEqual(report['reliability']['left']['storage'],'unfinalized',report)
        self.assertEqual(report['reliability']['left']['raw'],'unverified',report)
        self.assertEqual(report['reliability']['left']['provenance'],'unverified',report)
        self.assertFalse((unfinalized/'finalized.json').exists())
        self.assertEqual(before,{str(path.relative_to(unfinalized)):path.read_bytes()
                                 for path in unfinalized.rglob('*') if path.is_file()})

    def test_trace_v1_raw_receipt_counts_an_unterminated_final_line(self):
        directory=self.synthetic_trace_v1('trace-final-line-without-lf')
        trace=directory/'raw/trace.log'
        raw=trace.read_bytes()
        self.assertTrue(raw.endswith(b'\n'))
        raw=raw[:-1]
        trace.write_bytes(raw)
        manifest=json.loads((directory/'raw-integrity.json').read_bytes())
        manifest['receipt'].update(bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest())
        (directory/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(directory)
        shown=call(self.store,'show',directory.name,'--json')
        self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
        self.assert_trace_v1_wire(directory,json.loads(shown.stdout))
        report=self.trace_v5_report(directory.name,directory.name,0)
        self.assertEqual(report['reliability']['left']['raw'],'verified',report)

    def test_trace_v1_numeric_target_facts_match_the_saved_exit_spelling(self):
        accepted=[
            ('code-zero','+++ exited with 0 +++',0,None),
            ('code-max','+++ exited with 255 +++',255,None),
            ('term','+++ killed by SIGTERM +++',None,15),
            ('core-segv','+++ killed by SIGSEGV (core dumped) +++',None,11),
            ('rt-zero','+++ killed by SIGRT_0 +++',None,32),
            ('rt-max','+++ killed by SIGRT_32 +++',None,64),
            ('glibc-min-alias','+++ killed by SIGRTMIN +++',None,34),
            ('glibc-max-offset','+++ killed by SIGRTMIN+30 +++',None,64),
            ('rt-max-alias','+++ killed by SIGRTMAX +++',None,64),
            ('unknown-name','+++ killed by SIG_FUTURE +++',None,None),
            ('unknown-numeric','+++ killed by 99 +++',None,None),
        ]
        for label,outcome,code,signal in accepted:
            with self.subTest(label=label):
                directory=self.synthetic_trace_v1_outcome('trace-outcome-'+label,outcome,code,signal)
                shown=call(self.store,'show',directory.name,'--json')
                self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
                self.assert_trace_v1_wire(directory,json.loads(shown.stdout))
                report=self.trace_v5_report(directory.name,directory.name,
                                            0 if code is not None or signal is not None else 3)
                self.assertEqual(report['reliability']['left']['raw'],'verified',report)
                self.assertEqual(report['reliability']['left']['provenance'],'verified',report)

        contradicted=[
            ('raw-zero-code-seven','+++ exited with 0 +++',7,None),
            ('signal-as-shell-code','+++ killed by SIGTERM +++',143,None),
            ('known-signal-mismatch','+++ killed by SIGTERM +++',None,9),
            ('exit-as-signal','+++ exited with 0 +++',None,15),
            ('known-signal-number-missing','+++ killed by SIGTERM +++',None,None),
            ('unknown-name-number-invented','+++ killed by SIG_FUTURE +++',None,15),
            ('rt-offset-mismatch','+++ killed by SIGRT_2 +++',None,32),
            ('glibc-offset-outside-v1','+++ killed by SIGRTMIN+31 +++',None,64),
            ('malformed-known-outcome','success',0,None),
        ]
        for label,outcome,code,signal in contradicted:
            with self.subTest(label=label):
                directory=self.synthetic_trace_v1_outcome('trace-outcome-'+label,outcome,code,signal)
                shown=call(self.store,'show',directory.name,'--json')
                self.assertEqual(shown.returncode,2,shown.stderr+shown.stdout)
                report=self.trace_v5_report(directory.name,'trace-template',2)
                self.assertIsNone(report['observed_differences'],report)
                self.assertEqual(report['changes'],[],report)

        # V3 expressly did not decode this opaque field. New checks must never
        # reinterpret its persisted numeric facts using the trace-v1 grammar.
        legacy=self.synthetic_record('legacy-opaque-outcome')
        metadata=json.loads((legacy/'metadata.json').read_bytes())
        metadata.update(exit_code=7,collector_exit_code=7,target_exit_raw='+++ exited with 0 +++')
        (legacy/'metadata.json').write_text(json.dumps(metadata))
        self.refresh_marker(legacy)
        shown=call(self.store,'show',legacy.name,'--json')
        self.assertEqual(shown.returncode,0,shown.stderr+shown.stdout)
        self.assertEqual(json.loads(shown.stdout)['metadata']['exit_code'],7)
        compared=call(self.store,'diff',legacy.name,legacy.name,'--json')
        self.assertEqual(compared.returncode,0,compared.stderr+compared.stdout)
        self.assertEqual(json.loads(compared.stdout)['schema_version'],4)

    def test_v6_explicit_contract_keeps_automatic_formats_and_reliability(self):
        """Real captures freeze equal/changed statuses and deliberate opt-in views."""
        _,left=self.record_trace_v1('v6-real-left')
        _,equal=self.record_trace_v1('v6-real-equal')
        _,changed=self.record_trace_v1('v6-real-changed',('/usr/bin/false',),expected=1)
        for right,expected in [('v6-real-equal',0),('v6-real-changed',1)]:
            with self.subTest(right=right):
                report=self.trace_v6_report('v6-real-left',right,expected)
                self.assertEqual(report['observed_differences'],expected==1)
                automatic=call(self.store,'diff','v6-real-left',right,'--json')
                self.assertEqual(automatic.returncode,expected,automatic.stderr+automatic.stdout)
                automatic=json.loads(automatic.stdout)
                self.assertEqual(automatic['schema_version'],5)
                self.assertNotIn('evidence_contract',automatic)
                self.assertTrue(all('evidence_mapping' not in change for change in automatic['changes']))
        self.assertEqual(left['metadata']['target_provenance']['root']['generation'],1)
        self.assertEqual(equal['raw_integrity']['status'],'verified')
        self.assertEqual(changed['metadata']['exit_code'],1)
        old=self.trace_v6_report('healthy','healthy',3)
        self.assertFalse(old['observed_differences'])
        self.assertEqual(old['changes'],[])
        for side in ['left','right']:
            self.assertEqual(old['reliability'][side]['raw'],'unverified')
            self.assertEqual(old['reliability'][side]['provenance'],'unverified')
        default=call(self.store,'diff','healthy','healthy','--json')
        self.assertEqual(default.returncode,0,default.stderr+default.stdout)
        self.assertEqual(json.loads(default.stdout)['schema_version'],4)
        for first,second,old_side in [('healthy','v6-real-left','left'),('v6-real-left','healthy','right')]:
            with self.subTest(old_side=old_side):
                mixed=self.trace_v6_report(first,second,3)
                self.assertEqual(mixed['reliability'][old_side]['raw'],'unverified')
                self.assertEqual(mixed['reliability'][old_side]['provenance'],'unverified')
                self.assertIsNone(mixed['target_provenance'][old_side])

    def test_v6_full_outcome_mapping_survives_more_than_three_samples(self):
        """All errno occurrences survive bounded samples and typed-key grouping."""
        path='/baseline/name (openat)/payload (with parentheses)'
        left_specs=[dict(path=path,outcome=value) for value in
                    ['success']*5+['EACCES','EBUSY','EFAULT','EIO']]
        right_specs=[dict(path=path,outcome=value) for value in ['success']*4+['ENOENT']*3]
        left_specs += [dict(path='/baseline/removed',outcome='success')]*2
        right_specs += [dict(path='/baseline/new',outcome='success')]*2
        # Matching display subjects must not merge distinct operation/family keys.
        left_specs.append(dict(kind='exec',operation='execve',path='/baseline/failed (execve)',outcome='ENOENT'))
        right_specs.append(dict(kind='exec',operation='execve',path='/baseline/failed (execve)',outcome='EACCES'))
        right_specs.append(dict(operation='access',path=path,outcome='success'))
        directories=[self.synthetic_mapping_run('v6-many-left',left_specs),
                     self.synthetic_mapping_run('v6-many-right',right_specs)]
        left,right=[self.mapping_run(directory) for directory in directories]
        for directory,run in zip(directories,[left,right]):self.assert_trace_v1_wire(directory,run)
        report=self.trace_v6_report(directories[0].name,directories[1].name,1)
        self.assertEqual(len(report['changes']),5)
        for change in report['changes']:self.assert_v6_normalized_mapping(change,left,right)
        main=next(change for change in report['changes'] if change['evidence_mapping']['observation']==
                  dict(kind='file',operation='openat',path=path))
        facts=main['evidence_mapping']
        self.assertEqual(facts['left']['event_count'],9)
        self.assertEqual(facts['right']['event_count'],7)
        self.assertEqual(facts['left']['sample_event_indices'],[6,7,8])
        self.assertEqual(facts['right']['sample_event_indices'],[5,1,2])
        self.assertEqual(facts['left']['omitted_sample_count'],6)
        self.assertEqual(facts['right']['omitted_sample_count'],4)
        self.assertIn('EIO',[outcome['outcome'] for outcome in facts['left']['outcomes']])
        for category,missing_side in [('New file access','left'),('Removed file access','right')]:
            absent=next(change for change in report['changes'] if change['category']==category
                        and change['evidence_mapping']['observation']['path'] in ['/baseline/new','/baseline/removed'])
            self.assertEqual(absent['evidence_mapping'][missing_side]['event_count'],0)
            self.assertEqual(absent['evidence_mapping'][missing_side]['omitted_sample_count'],0)

    def test_v6_duplicate_coordinates_and_resumed_spans_keep_vector_indices(self):
        """One saved coordinate can support distinct occurrences without index loss."""
        path='/baseline/coordinate (payload)'
        left_specs=[dict(path=path,outcome='EACCES',coordinates=(5,5)),
                    dict(path=path,outcome='success',coordinates=(2,4)),
                    dict(path=path,outcome='ENOENT',coordinates=(3,3)),
                    dict(path=path,outcome='EACCES',coordinates=(5,5))]
        right_specs=[dict(path=path,outcome='success',coordinates=(2,4)),
                     dict(path=path,outcome='EIO',coordinates=(5,5))]
        directories=[self.synthetic_mapping_run('v6-coordinate-left',left_specs),
                     self.synthetic_mapping_run('v6-coordinate-right',right_specs)]
        left,right=[self.mapping_run(directory) for directory in directories]
        report=self.trace_v6_report(directories[0].name,directories[1].name,1)
        self.assertEqual(len(report['changes']),1)
        change=report['changes'][0]
        self.assert_v6_normalized_mapping(change,left,right)
        outcomes=change['evidence_mapping']['left']['outcomes']
        repeated=next(outcome for outcome in outcomes if outcome['outcome']=='EACCES')
        self.assertEqual([reference['index'] for reference in repeated['events']],[1,4])
        self.assertEqual(repeated['events'][0]['evidence'],repeated['events'][1]['evidence'])
        resumed=next(outcome for outcome in outcomes if outcome['outcome']=='success')
        self.assertEqual(resumed['events'],[dict(index=2,evidence=dict(file='raw/trace.log',line=2,end_line=4))])
        self.assertEqual(change['evidence_mapping']['left']['sample_event_indices'],[1,3,2])
        self.assertEqual(change['evidence_mapping']['left']['omitted_sample_count'],1)

    def test_v6_pid_counts_and_scheduling_remain_outside_comparison_identity(self):
        """Different PIDs, repetition and vector order do not create new changes."""
        path='/baseline/identity-control'
        left_specs=[dict(path=path,outcome='success'),dict(path=path,outcome='ENOENT')]
        right_specs=[dict(path=path,outcome='ENOENT')]*4+[dict(path=path,outcome='success')]*2
        left=self.synthetic_mapping_run('v6-identity-left',left_specs,pid='41')
        right=self.synthetic_mapping_run('v6-identity-right',right_specs,pid='99')
        left_run,right_run=self.mapping_run(left),self.mapping_run(right)
        self.assertNotEqual(left_run['events'][1]['pid'],right_run['events'][1]['pid'])
        self.assertNotEqual(len(left_run['events']),len(right_run['events']))
        report=self.trace_v6_report(left.name,right.name,0)
        self.assertFalse(report['observed_differences'])
        self.assertEqual(report['changes'],[])

    def test_v6_target_references_and_metadata_changes_have_distinct_authorities(self):
        """Only target provenance attributes exits; metadata never borrows syscalls."""
        left=self.synthetic_trace_v1('v6-metadata-left')
        right=self.synthetic_trace_v1_outcome('v6-metadata-right','+++ exited with 7 +++',7,None)
        metadata=json.loads((right/'metadata.json').read_bytes())
        metadata.update(command=['/usr/bin/true','--metadata-control'],cwd='/other/lexical/context',
                        environment={'V6_SELECTED':'value'},warnings=['synthetic metadata limitation'],completeness='partial')
        (right/'metadata.json').write_text(json.dumps(metadata))
        self.refresh_trace_v1_marker(right)
        runs=[self.mapping_run(directory) for directory in [left,right]]
        report=self.trace_v6_report(left.name,right.name,3)
        changes={change['category']:change for change in report['changes']}
        self.assertEqual(set(changes),{'Exit status','Command arguments','Initial working directory',
                                      'Selected startup environment','Capture completeness'})
        for side,run in zip(['left','right'],runs):self.assert_v6_target_side(changes['Exit status'],side,run)
        fields={'Command arguments':['metadata.command'],'Initial working directory':['metadata.cwd'],
                'Selected startup environment':['metadata.environment'],
                'Capture completeness':['metadata.state','metadata.completeness','metadata.warnings']}
        for category,compared in fields.items():
            self.assertIsNone(changes[category]['evidence_mapping']['observation'])
            for side in ['left','right']:self.assert_v6_metadata_side(changes[category],side,compared)
        old=self.trace_v6_report('healthy',right.name,3)
        target=next(change for change in old['changes'] if change['category']=='Exit status')
        self.assert_v6_metadata_side(target,'left',['metadata.exit_code','metadata.signal','metadata.target_exit_raw'])
        self.assert_v6_target_side(target,'right',runs[1])
        unknown=[self.synthetic_trace_v1_outcome('v6-unknown-name','+++ killed by SIG_FUTURE +++'),
                 self.synthetic_trace_v1_outcome('v6-unknown-number','+++ killed by 99 +++')]
        unknown_runs=[self.mapping_run(directory) for directory in unknown]
        different=self.trace_v6_report(unknown[0].name,unknown[1].name,3)
        target=next(change for change in different['changes'] if change['category']=='Exit status')
        for side,run in zip(['left','right'],unknown_runs):self.assert_v6_target_side(target,side,run)

    def test_v6_unfinalized_observations_are_withheld_without_invented_counts(self):
        """Withheld vectors cannot authorize saved counts or target samples.

        This is the sole named v5 projection exception: v5 exposed a persisted
        target coordinate without finalization, while explicit v6 withholds it.
        """
        path='/baseline/withheld-control'
        left=self.synthetic_mapping_run('v6-withheld-left',[dict(path=path,outcome='success')])
        right=self.synthetic_mapping_run('v6-withheld-right',[dict(path=path,outcome='success')])
        metadata=json.loads((left/'metadata.json').read_bytes())
        metadata['command'].append('--metadata-control')
        (left/'metadata.json').write_text(json.dumps(metadata))
        self.refresh_trace_v1_marker(left)
        metadata=json.loads((right/'metadata.json').read_bytes())
        events=[json.loads(line) for line in (right/'events.jsonl').read_bytes().splitlines()]
        metadata.update(exit_code=7,collector_exit_code=7,target_exit_raw='+++ exited with 7 +++')
        events[-1]['outcome']=metadata['target_exit_raw']
        (right/'metadata.json').write_text(json.dumps(metadata))
        (right/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
        raw=(right/'raw/trace.log').read_bytes().replace(b'+++ exited with 0 +++',b'+++ exited with 7 +++')
        (right/'raw/trace.log').write_bytes(raw)
        manifest=json.loads((right/'raw-integrity.json').read_bytes())
        manifest['receipt'].update(bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest())
        (right/'raw-integrity.json').write_text(json.dumps(manifest))
        self.refresh_trace_v1_marker(right)
        (left/'finalized.json').unlink()
        before={str(path.relative_to(left)):path.read_bytes() for path in left.rglob('*') if path.is_file()}
        left_run,right_run=self.mapping_run(left),self.mapping_run(right)
        self.assertEqual(left_run['events'],[])
        self.assertGreater(left_run['metadata']['parsed_events'],0)
        evidence=left_run['metadata']['target_provenance']['target_exit']['evidence']
        report=self.trace_v6_report(left.name,right.name,3,withheld_target=('left',evidence))
        target=next(change for change in report['changes'] if change['category']=='Exit status')
        fields=['metadata.exit_code','metadata.signal','metadata.target_exit_raw']
        self.assert_v6_metadata_side(target,'left',fields,source='withheld')
        self.assert_v6_target_side(target,'right',right_run)
        command=next(change for change in report['changes'] if change['category']=='Command arguments')
        self.assert_v6_metadata_side(command,'left',['metadata.command'],source='withheld')
        self.assert_v6_metadata_side(command,'right',['metadata.command'])
        observed=next(change for change in report['changes'] if change['evidence_mapping']['observation']==
                      dict(kind='file',operation='openat',path=path))
        self.assert_v6_normalized_mapping(observed,left_run,right_run)
        self.assertEqual(report['reliability']['left']['storage'],'unfinalized')
        self.assertEqual(report['reliability']['left']['raw'],'unverified')
        self.assertEqual(report['reliability']['left']['provenance'],'unverified')
        self.assertEqual(before,{str(path.relative_to(left)):path.read_bytes() for path in left.rglob('*') if path.is_file()})
        self.assertFalse((left/'finalized.json').exists())

    def test_v6_errors_do_not_emit_ordinary_changes_or_invalid_references(self):
        """Raw errors and self-consistent corrupt DTOs fail without a comparison."""
        raw_bad=self.synthetic_trace_v1('v6-corrupt-raw')
        raw=(raw_bad/'raw/trace.log').read_bytes()
        (raw_bad/'raw/trace.log').write_bytes(bytes([raw[0]^1])+raw[1:])
        both=[self.trace_v1_copy('v6-structured-'+str(index)) for index in range(2)]
        for directory in both:(directory/'metadata.json').write_bytes(b'{broken')
        for first,second,raw_failure in [(raw_bad.name,'trace-template',True),(both[0].name,both[1].name,False)]:
            with self.subTest(first=first):
                report=self.trace_v6_report(first,second,2)
                self.assertIsNone(report['observed_differences'])
                self.assertEqual(report['changes'],[])
                self.assertTrue(report['errors'])
                if raw_failure:
                    self.assertEqual(report['reliability']['left']['storage'],'verified')
                    self.assertEqual(report['reliability']['left']['raw'],'corrupt')
                else:self.assertGreaterEqual(len(report['errors']),2)
        for mutation in ['target-index','pathless-event','numeric-contradiction']:
            with self.subTest(mutation=mutation):
                directory=self.synthetic_mapping_run('v6-invalid-'+mutation,
                                [dict(path='/baseline/invalid-control',outcome='success')])
                metadata=json.loads((directory/'metadata.json').read_bytes())
                events=[json.loads(line) for line in (directory/'events.jsonl').read_bytes().splitlines()]
                if mutation=='target-index':metadata['target_provenance']['target_exit']['index']=0
                elif mutation=='pathless-event':events[1]['path']=None
                else:metadata.update(exit_code=7,collector_exit_code=7)
                (directory/'metadata.json').write_text(json.dumps(metadata))
                (directory/'events.jsonl').write_bytes(b''.join((json.dumps(event)+'\n').encode() for event in events))
                self.refresh_trace_v1_marker(directory)
                # The marker hashes match every edited structured byte, so the
                # counterexample challenges semantic validation, not the hash.
                marker=json.loads((directory/'finalized.json').read_bytes())
                for key,filename in [('metadata','metadata.json'),('events','events.jsonl')]:
                    data=(directory/filename).read_bytes()
                    self.assertEqual(marker[key],dict(bytes=len(data),sha256=hashlib.sha256(data).hexdigest()))
                shown=self.mapping_call('show',directory.name,'--json')
                self.assertEqual(shown.returncode,2,shown.stderr+shown.stdout)
                report=self.trace_v6_report(directory.name,'trace-template',2)
                self.assertIsNone(report['observed_differences'])
                self.assertEqual(report['changes'],[])

class StorageLockCLI(unittest.TestCase):
    """No capture: advisory-lock teardown has no lock-file data to persist."""

    def test_lock_drop_does_not_sync_advisory_inode(self):
        import fcntl
        work=WORK/self._testMethodName
        work.mkdir()
        compiler=shutil.which('cc')
        self.assertIsNotNone(compiler,'cc required for deterministic lock-fsync regression')
        source=work/'lock-fsync.c'
        library=work/'lock-fsync.so'
        source.write_text(r'''
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

/* Observe only the preselected inode; record/marker syncs keep their real syscall. */
static void mark(const char *message) {
    /* Lost audit bytes must fail the child, never make a sync call look absent. */
    const char *path=getenv("RUNDELTA_TEST_FSYNC_LOG");
    if (!path) _exit(93);
    int fd=(int)syscall(SYS_openat,AT_FDCWD,path,O_WRONLY|O_CREAT|O_APPEND|O_CLOEXEC,0600);
    if (fd<0) _exit(93);
    size_t n=0; while (message[n]) ++n;
    size_t offset=0;
    while (offset<n) {
        ssize_t written=syscall(SYS_write,fd,message+offset,n-offset);
        if (written<0 && errno==EINTR) continue;
        if (written<=0) _exit(93);
        offset+=(size_t)written;
    }
    if (syscall(SYS_close,fd)<0) _exit(93);
}
__attribute__((constructor)) static void loaded(void) { mark("loaded\n"); }
int fsync(int fd) {
    struct stat st;
    const char *dev=getenv("RUNDELTA_TEST_FSYNC_DEV");
    const char *ino=getenv("RUNDELTA_TEST_FSYNC_INO");
    if (dev && ino && fstat(fd,&st)==0 && (uintmax_t)st.st_dev==strtoumax(dev,NULL,10)
        && (uintmax_t)st.st_ino==strtoumax(ino,NULL,10)) {
        mark("lock-fsync\n");
        errno=EIO;
        return -1;
    }
    return (int)syscall(SYS_fsync,fd);
}
''')

        def execute(label,argv,env):
            # Retain exact commands and raw bytes even when an assertion fails.
            (work/(label+'.command.json')).write_text(json.dumps(dict(
                argv=argv,environment=env,source_sha256=hashlib.sha256(source.read_bytes()).hexdigest(),
                binary_sha256=hashlib.sha256(BIN.read_bytes()).hexdigest(),
                library_sha256=hashlib.sha256(library.read_bytes()).hexdigest()
                if library.exists() else None),indent=2))
            result=subprocess.run(argv,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
            (work/(label+'.stdout')).write_bytes(result.stdout)
            (work/(label+'.stderr')).write_bytes(result.stderr)
            (work/(label+'.result.json')).write_text(json.dumps(dict(returncode=result.returncode)))
            LOG.append(dict(args=argv,returncode=result.returncode,stderr=result.stderr.decode(errors='replace')))
            return result

        compiled=execute('compile',[compiler,'-shared','-fPIC','-Wall','-Wextra','-Werror',
                         '-o',str(library),str(source)],dict(ENV))
        self.assertEqual(compiled.returncode,0,compiled.stderr)
        for case in ['missing','delete']:
            with self.subTest(case=case):
                store=work/case
                (store/'runs').mkdir(parents=True)
                lock=store/'.record.lock'
                lock.write_bytes(b'')
                lock_stat=lock.stat()
                if case=='delete':(store/'runs'/'abc123-789').mkdir()
                (work/(case+'.input.json')).write_text(json.dumps(dict(
                    store=str(store),lock_dev=lock_stat.st_dev,lock_ino=lock_stat.st_ino,
                    lock_sha256=hashlib.sha256(lock.read_bytes()).hexdigest(),
                    records=['abc123-789'] if case=='delete' else [],record_entries=[]),indent=2))
                env=dict(ENV,LD_PRELOAD=str(library),RUNDELTA_TEST_FSYNC_DEV=str(lock_stat.st_dev),
                         RUNDELTA_TEST_FSYNC_INO=str(lock_stat.st_ino))
                # A known call on the same inode proves this shim actually intercepts fsync.
                control_log=work/(case+'-control.events')
                env['RUNDELTA_TEST_FSYNC_LOG']=str(control_log)
                control=execute(case+'-control',[sys.executable,'-B','-c',
                    'import errno,os,sys; fd=os.open(sys.argv[1],os.O_RDWR)\n'
                    'try:\n'
                    ' try: os.fsync(fd)\n'
                    ' except OSError as e:\n'
                    '  if e.errno!=errno.EIO: raise AssertionError(("unexpected errno",e.errno))\n'
                    ' else: raise AssertionError("fsync observer did not reject the selected inode")\n'
                    'finally: os.close(fd)\n',str(lock)],env)
                self.assertEqual(control.returncode,0,control.stderr)
                self.assertEqual(control_log.read_text().splitlines(),['loaded','lock-fsync'])
                cli_log=work/(case+'-cli.events')
                env['RUNDELTA_TEST_FSYNC_LOG']=str(cli_log)
                selector='missing' if case=='missing' else 'abc123-789'
                result=execute(case+'-cli',[str(BIN),'--storage',str(store),'delete',selector],env)
                self.assertEqual(result.returncode,2 if case=='missing' else 0,result.stderr)
                self.assertEqual(result.stdout,b'' if case=='missing' else b'Deleted abc123-789\n')
                if case=='missing':self.assertIn(b'expected one safe ID/label match, found 0',result.stderr)
                else:self.assertEqual(result.stderr,b'')
                self.assertEqual(list((store/'runs').iterdir()),[])
                self.assertEqual(lock.read_bytes(),b'')
                # Both success and error-return destructors must release the owned lock.
                with lock.open('rb') as available:
                    fcntl.flock(available,fcntl.LOCK_EX|fcntl.LOCK_NB)
                    fcntl.flock(available,fcntl.LOCK_UN)
                self.assertEqual(cli_log.read_text().splitlines(),['loaded'],
                                 'Lock::Drop must not fsync its advisory inode')

if __name__=='__main__':
    try:main()
    finally:
        (WORK/'commands.json').write_text(json.dumps(LOG,indent=2))
        print('ARTIFACTS:',WORK)
