#!/usr/bin/env python3
"""Synchronized external rename; only fresh test records are created."""
import json, os, pathlib, select, subprocess, tempfile, unittest
from acceptance_support import strace_environment
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=str(pathlib.Path(os.environ.get('RUNDELTA_BIN',ROOT/'target/debug/rundelta')).resolve())
ENV=strace_environment(ROOT)
WORK=pathlib.Path(tempfile.mkdtemp(prefix='cwd-uncertainty-',dir=ROOT/'target'))
PROBE=WORK/'probe'
subprocess.run(['cc',str(ROOT/'tests/fixtures/cwd_probe.c'),'-o',str(PROBE)],check=True)
class Cwd(unittest.TestCase):
    def experiment(self,mode):
        w=WORK/mode;w.mkdir();old=w/'old';old.mkdir();(old/'file').write_text('payload')
        storage=w/'records'
        with (w/'stderr.txt').open('w') as err:
            p=subprocess.Popen([BIN,'--storage',str(storage),'record',mode,'--',str(PROBE),str(old),mode],env=ENV,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err)
            try:
                self.assertTrue(select.select([p.stdout],[],[],5)[0],'missing synchronization')
                self.assertEqual(p.stdout.readline(),b'CWD_READY\n')
                if mode=='rename':old.rename(w/'new')
                # rename() has returned before allowing either relative syscall.
                stdout,_=p.communicate(b'G',timeout=10)
                self.assertEqual(p.returncode,0,(w/'stderr.txt').read_text())
                self.assertIn(b'ACCESS_OK',stdout)
            finally:
                if p.poll() is None:p.kill();p.wait(timeout=5)
        def cli(*args):return subprocess.run([BIN,'--storage',str(storage),*args],env=ENV,capture_output=True,text=True,timeout=10)
        r=cli('show',mode,'--json');self.assertEqual(r.returncode,0,r.stderr)
        data=json.loads(r.stdout);(w/'show.json').write_text(r.stdout)
        if mode=='failed-chdir':
            failed=next(e for e in data['events']
                        if e['operation']=='chdir' and e['outcome']!='success')
            self.assertEqual(failed['path'],'relative:missing [cwd unresolved]')
            self.assertTrue(failed['context_uncertain'])
            prefix=f"raw/trace.log:{failed['evidence']['line']}: path context unresolved;"
            self.assertTrue(any(warning.startswith(prefix)
                                for warning in data['metadata']['warnings']),
                            data['metadata']['warnings'])
        events=[e for e in data['events']
                if e['kind']=='file' and e['operation'] in ('open','access')
                and ((e['path'] or '').endswith('/file')
                     or (e['path'] or '').startswith('relative:file '))]
        self.assertEqual(len(events),2,events)
        raw=(storage/'runs'/data['metadata']['id']/'raw/trace.log').read_text().splitlines()
        for e in events:
            self.assertEqual(e['outcome'],'success')
            self.assertIn('\\x66\\x69\\x6c\\x65',raw[e['evidence']['line']-1])
            self.assertEqual(e['cwd_before'],str(old),'last observed context must survive failed switches/rename')
        access=next(e for e in events if e['operation']=='access')
        self.assertTrue(access['path'].startswith('relative:file '),access)
        self.assertTrue(access['context_uncertain'],access)
        self.assertNotEqual(access['path'],str(old/'file'))
        opened=next(e for e in events if e['operation']=='open')
        self.assertEqual(opened['path'],str(w/('new' if mode=='rename' else 'old')/'file'))
        self.assertFalse(opened['context_uncertain'])
        text=cli('show',mode).stdout
        self.assertIn(access['path'],text)
        self.assertIn('Last observed CWD',text)
        d=cli('diff',mode,mode,'--json');t=cli('diff',mode,mode)
        self.assertEqual(d.returncode,3);self.assertEqual(t.returncode,3)
        report=json.loads(d.stdout);self.assertFalse(report['observed_differences'])
        self.assertEqual(report['reliability']['left'],{'capture':'incomplete','storage':'verified'})
        self.assertIn('RELIABILITY NOT ESTABLISHED',t.stdout)
        self.assertNotIn('rename(', '\n'.join(raw))
    def test_normal(self):self.experiment('normal')
    def test_external_rename(self):self.experiment('rename')
    def test_failed_chdir(self):self.experiment('failed-chdir')
    def test_failed_fchdir(self):self.experiment('failed-fchdir')
if __name__=='__main__':
    from acceptance_support import main
    try:main()
    finally:print('ARTIFACTS:',WORK)
