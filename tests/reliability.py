#!/usr/bin/env python3
"""Regression gates for the three acceptance findings; uses real strace captures."""
import importlib.util
import ctypes, json, os, pathlib, select, signal, struct, subprocess, sys, tempfile, time, unittest
from contextlib import ExitStack, contextmanager
from unittest import mock
from acceptance_support import main, strace_environment
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=pathlib.Path(os.environ.get('RUNDELTA_BIN',str(ROOT/'target/debug/rundelta')))
ENV=strace_environment(ROOT)
WORK=pathlib.Path(tempfile.mkdtemp(prefix='reliability-',dir=ROOT/'target'))
PROBE=WORK/'probe'
subprocess.run(['cc',str(ROOT/'tests/fixtures/audit_probe.c'),'-o',str(PROBE)],check=True)
TRANSCRIPT=[]

def run(store,*args):
    cmd=[str(BIN),'--storage',str(store),*map(str,args)]
    p=subprocess.run(cmd,env=ENV,capture_output=True,text=True,timeout=10)
    TRANSCRIPT.append(dict(command=cmd,returncode=p.returncode,stdout=p.stdout,stderr=p.stderr))
    return p

def parse_ready_pids(contents, expected):
    if not contents.endswith("\n") or contents.count("\n") != 1:
        raise ValueError("incomplete readiness PID marker")
    fields = contents[:-1].split(" ")
    if len(fields) != expected or any(not value.isascii() or not value.isdecimal() or int(value) <= 0 for value in fields):
        raise ValueError("invalid readiness PID fields")
    pids = [int(value) for value in fields]
    if len(set(pids)) != len(pids):
        raise ValueError("readiness PID fields must be distinct")
    return pids


def pidfd_exited(descriptor):
    """Observe the original process, including termination before it is reaped."""
    return bool(select.select([descriptor], [], [], 0)[0])


@contextmanager
def observe_processes(process_ids):
    """Retain each acquired identity even when a later acquisition fails."""
    handles = []
    with ExitStack() as cleanup:
        for process_id in process_ids:
            descriptor = os.pidfd_open(process_id)
            handles.append(descriptor)
            cleanup.callback(os.close, descriptor)
            cleanup.callback(stop_observed_process, descriptor)
        yield handles


def stop_observed_process(descriptor):
    """Best effort only for an already vanished original identity."""
    try:
        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
    except ProcessLookupError:
        pass



class ReadinessPublication(unittest.TestCase):
    def watch_moves(self, directory, target_name, mask=0x00000080):
        libc = ctypes.CDLL(None, use_errno=True)
        libc.inotify_init1.argtypes = [ctypes.c_int]
        libc.inotify_init1.restype = ctypes.c_int
        libc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
        libc.inotify_add_watch.restype = ctypes.c_int
        descriptor = libc.inotify_init1(os.O_CLOEXEC | os.O_NONBLOCK)
        if descriptor < 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        self.addCleanup(os.close, descriptor)
        watch = libc.inotify_add_watch(descriptor, os.fsencode(directory), mask)
        if watch < 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))
        return descriptor, target_name

    def assert_moved_to(self, watcher, timeout=2, mask=0x00000080):
        descriptor, target_name = watcher
        self.assertTrue(select.select([descriptor], [], [], timeout)[0], "atomic publication event timed out")
        data = os.read(descriptor, 4096)
        offset = 0
        while offset + 16 <= len(data):
            watch, event_mask, cookie, length = struct.unpack_from("iIII", data, offset)
            name = data[offset + 16:offset + 16 + length].split(b"\0", 1)[0].decode()
            if event_mask & mask and name == target_name:
                return
            offset += 16 + length
        self.fail("target marker publication event was not observed")

    def test_python_backend_marker_is_complete_on_create(self):
        with tempfile.TemporaryDirectory(prefix="python-marker-") as directory:
            target = pathlib.Path(directory) / "ready"
            watcher = self.watch_moves(directory, target.name, mask=0x00000100)
            environment = os.environ.copy()
            environment.update({"RF440_READY": str(target), "RF440_PROBE_MODE": "success"})
            completed = subprocess.run(
                [sys.executable, str(ROOT / "tests/fixtures/version_backend.py"), "--version"],
                env=environment, capture_output=True, timeout=3,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assert_moved_to(watcher, mask=0x00000100)
            parse_ready_pids(target.read_text(encoding="ascii"), 1)
            self.assertEqual(list(pathlib.Path(directory).iterdir()), [target])

    def test_python_marker_preserves_existing_entries_and_cleans_failures(self):
        spec = importlib.util.spec_from_file_location("version_backend", ROOT / "tests/fixtures/version_backend.py")
        backend = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(backend)
        with tempfile.TemporaryDirectory(prefix="python-marker-failures-") as directory:
            root = pathlib.Path(directory)
            target = root / "ready"
            original = root / "original"
            original.write_text("keep\n", encoding="ascii")
            with mock.patch.dict(os.environ, {"RF440_READY": str(target)}):
                for symlink in (False, True):
                    with self.subTest(symlink=symlink):
                        if symlink:
                            target.symlink_to(original)
                        else:
                            target.write_text("keep\n", encoding="ascii")
                        with self.assertRaises(FileExistsError):
                            backend.write_marker("RF440_READY", "123\n")
                        self.assertEqual(target.is_symlink(), symlink)
                        self.assertEqual(target.read_text(), "keep\n")
                        self.assertEqual(original.read_text(), "keep\n")
                        self.assertEqual(set(root.iterdir()), {target, original})
                        target.unlink()
                with mock.patch.object(backend.os, "link", side_effect=OSError("publish failed")):
                    with self.assertRaisesRegex(OSError, "publish failed"):
                        backend.write_marker("RF440_READY", "123\n")
                self.assertEqual(list(root.iterdir()), [original])
                with mock.patch.object(backend.os, "link", side_effect=OSError("primary publish failure")), \
                     mock.patch.object(backend.Path, "unlink", side_effect=OSError("cleanup failed")):
                    with self.assertRaisesRegex(OSError, "primary publish failure"):
                        backend.write_marker("RF440_READY", "123\n")
                # The injected unlink failure deliberately leaves a temp; remove it
                # after restoring unlink so this regression has no lasting files.
                for temporary in root.glob(".ready.*.tmp"):
                    temporary.unlink()
                self.assertEqual(list(root.iterdir()), [original])

    def test_c_pid_pair_is_complete_and_existing_marker_is_not_replaced(self):
        with tempfile.TemporaryDirectory(prefix="c-marker-") as directory:
            root = pathlib.Path(directory)
            ready = root / "tree.ready"
            watcher = self.watch_moves(root, ready.name)
            process = subprocess.Popen([str(PROBE), "wait-tree", str(ready)], start_new_session=True)
            try:
                self.assert_moved_to(watcher)
                pids = parse_ready_pids(ready.read_text(encoding="ascii"), 2)
                self.assertEqual(pids[0], process.pid)
                self.assertNotEqual(pids[0], pids[1])
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
            preserved = root / "preserved.ready"
            preserved.write_text("keep\n", encoding="ascii")
            refused = subprocess.run([str(PROBE), "wait", str(preserved)], timeout=3)
            self.assertEqual(refused.returncode, 4)
            self.assertEqual(preserved.read_text(encoding="ascii"), "keep\n")

class ObserverLifetime(unittest.TestCase):
    def test_partial_acquisition_stops_original_child_and_closes_handle(self):
        process = subprocess.Popen(['python3', '-c', 'import time; time.sleep(30)'])
        observer = os.pidfd_open(process.pid)
        acquired = []
        real_open = os.pidfd_open

        def fail_second(process_id):
            if acquired:
                raise OSError('injected second pidfd acquisition failure')
            descriptor = real_open(process_id)
            acquired.append(descriptor)
            return descriptor

        try:
            with mock.patch.object(os, 'pidfd_open', side_effect=fail_second):
                with self.assertRaisesRegex(OSError, 'second pidfd acquisition'):
                    with observe_processes([process.pid, process.pid]):
                        self.fail('partial observer set reached lifecycle operations')
            self.assertTrue(select.select([observer], [], [], 2)[0],
                            'partial acquisition lost original child cleanup authority')
            self.assertEqual(len(acquired), 1)
            with self.assertRaises(OSError):
                os.fstat(acquired[0])
        finally:
            try:
                signal.pidfd_send_signal(observer, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=3)
            os.close(observer)

    def test_successful_observers_keep_original_exit_and_close_every_handle(self):
        process = subprocess.Popen(['python3', '-c', 'import time; time.sleep(30)'])
        observer = os.pidfd_open(process.pid)
        handles = []
        try:
            with observe_processes([process.pid, process.pid]) as handles:
                self.assertFalse(any(pidfd_exited(fd) for fd in handles))
                signal.pidfd_send_signal(observer, signal.SIGTERM)
                self.assertTrue(select.select([observer], [], [], 2)[0])
                self.assertTrue(all(pidfd_exited(fd) for fd in handles))
            self.assertEqual(len(handles), 2)
            for descriptor in handles:
                with self.assertRaises(OSError):
                    os.fstat(descriptor)
        finally:
            try:
                signal.pidfd_send_signal(observer, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=3)
            os.close(observer)

    def test_cleanup_signal_error_still_closes_every_acquired_handle(self):
        process = subprocess.Popen(['python3', '-c', 'import time; time.sleep(30)'])
        observer = os.pidfd_open(process.pid)
        handles = []
        try:
            with mock.patch.object(signal, 'pidfd_send_signal',
                                   side_effect=PermissionError('injected observer signal refusal')) as send:
                with self.assertRaisesRegex(PermissionError, 'signal refusal'):
                    with observe_processes([process.pid, process.pid]) as handles:
                        self.assertEqual(len(handles), 2)
            self.assertEqual(send.call_count, 2)
            for descriptor in handles:
                with self.assertRaises(OSError):
                    os.fstat(descriptor)
        finally:
            signal.pidfd_send_signal(observer, signal.SIGKILL)
            process.wait(timeout=3)
            os.close(observer)


class Reliability(unittest.TestCase):
    def setUp(self):
        self.store=WORK/self._testMethodName
        p=run(self.store,'record','base','--',PROBE,'ok')
        self.assertEqual(p.returncode,0,p.stderr)
        self.directory=next((self.store/'runs').iterdir())

    def test_storage_corruption(self):
        events=self.directory/'events.jsonl';original=events.read_bytes()
        mutations={'whole_line':b'\n'.join(original.splitlines()[:-1])+b'\n',
                   'half_line':original[:-8], 'missing':None,
                   'same_size_content':original.replace(b'success',b'succesz',1)}
        for kind,data in mutations.items():
            with self.subTest(kind=kind):
                if data is None:events.unlink()
                else:events.write_bytes(data)
                try:
                    p=run(self.store,'diff','base','base','--json')
                    self.assertEqual(p.returncode,2,p.stdout+p.stderr)
                    r=json.loads(p.stdout)
                    self.assertIsNone(r['observed_differences'])
                    self.assertEqual(r['reliability']['left']['storage'],'corrupt')
                    self.assertTrue(r['errors'])
                    show=run(self.store,'show','base')
                    self.assertEqual(show.returncode,2)
                finally:events.write_bytes(original)

    def test_finalization_and_legacy(self):
        marker=self.directory/'finalized.json'
        self.assertTrue(marker.exists(),'final completion marker missing')
        saved=marker.read_bytes();marker.unlink()
        p=run(self.store,'diff','base','base','--json')
        self.assertEqual(p.returncode,3,p.stdout+p.stderr)
        self.assertEqual(json.loads(p.stdout)['reliability']['left']['storage'],'unfinalized')
        marker.write_bytes(saved)
        p=run(self.store,'diff','base','base','--json')
        self.assertEqual(p.returncode,0)
        self.assertEqual(json.loads(p.stdout)['reliability']['left']['storage'],'verified')
        marker.unlink()
        meta=self.directory/'metadata.json';m=json.loads(meta.read_text());m['schema_version']=2;meta.write_text(json.dumps(m))
        p=run(self.store,'diff','base','base','--json')
        self.assertEqual(p.returncode,3)
        self.assertEqual(json.loads(p.stdout)['reliability']['left']['storage'],'unverified')
        self.assertEqual(run(self.store,'record','fresh','--',PROBE,'ok').returncode,0)
        compared=run(self.store,'diff','base','fresh','--json')
        self.assertEqual(compared.returncode,3)
        self.assertIs(json.loads(compared.stdout)['observed_differences'],False,
                      'storage uncertainty must not invent a capture/behavior difference')


    def test_realtime_signal(self):
        for sig in [signal.SIGTERM,signal.SIGKILL,signal.SIGRTMIN,signal.SIGRTMAX]:
            with self.subTest(signal=sig):
                name=f'sig-{int(sig)}'
                p=run(self.store,'record',name,'--',PROBE,'signal',int(sig))
                self.assertEqual(p.returncode,128+int(sig),p.stderr)
                m=json.loads(run(self.store,'show',name,'--json').stdout)['metadata']
                self.assertEqual(m['state'],'completed')
                self.assertEqual(m['signal'],int(sig));self.assertIsNone(m['exit_code'])
        p=run(self.store,'record','numeric','--',PROBE,'exit143')
        self.assertEqual(p.returncode,143)
        m=json.loads(run(self.store,'show','numeric','--json').stdout)['metadata']
        self.assertEqual(m['exit_code'],143);self.assertIsNone(m['signal'])
        for sig in [signal.SIGRTMIN,signal.SIGRTMAX]:
            code=128+int(sig);name=f'numeric-{code}'
            p=run(self.store,'record',name,'--',PROBE,'exit-code',code)
            self.assertEqual(p.returncode,code)
            m=json.loads(run(self.store,'show',name,'--json').stdout)['metadata']
            self.assertEqual(m['exit_code'],code);self.assertIsNone(m['signal'])


    def test_sigstkflt_signal_contract(self):
        sig=int(signal.SIGSTKFLT)
        self.assertEqual(sig,16,'Linux SIGSTKFLT ABI changed unexpectedly')
        recorded=run(self.store,'record','sigstkflt','--',PROBE,'signal',sig)
        self.assertEqual(recorded.returncode,128+sig,recorded.stderr)

        shown=run(self.store,'show','sigstkflt','--json')
        self.assertEqual(shown.returncode,0,shown.stderr)
        result=json.loads(shown.stdout);m=result['metadata']
        self.assertEqual(m['state'],'completed')
        self.assertEqual(m['completeness'],'complete_for_supported_events')
        self.assertEqual((m['exit_code'],m['signal']),(None,sig))
        self.assertEqual((m['collector_exit_code'],m['collector_signal']),(None,sig))
        self.assertEqual(m['target_exit_raw'],'+++ killed by SIGSTKFLT +++')
        self.assertEqual(m['warnings'],[])
        self.assertEqual(result['storage_integrity']['status'],'verified')

        compared=run(self.store,'diff','sigstkflt','sigstkflt','--json')
        self.assertEqual(compared.returncode,0,compared.stderr)
        report=json.loads(compared.stdout)
        self.assertEqual(report['exit_code'],0)
        self.assertIs(report['observed_differences'],False)
        self.assertEqual(report['changes'],[])
        self.assertEqual(report['errors'],[])
        self.assertEqual(report['warnings'],[])
        expected={'capture':'complete','storage':'verified'}
        self.assertEqual(report['reliability'],{'left':expected,'right':expected})


    def test_recorder_lifecycle(self):
        for mode in ['wait','wait-tree','wait-detached']:
            for sig in [signal.SIGTERM,signal.SIGKILL]:
                with self.subTest(mode=mode,signal=sig):
                    ready=WORK/f'{mode}-{int(sig)}.ready'
                    with (WORK/f'{mode}-{int(sig)}.log').open('w') as log:
                        p=subprocess.Popen([str(BIN),'--storage',str(self.store),'record',ready.stem,'--',str(PROBE),mode,str(ready)],env=ENV,stdout=log,stderr=log)
                        try:
                            deadline=time.monotonic()+5
                            expected_targets = 1 if mode == 'wait' else 2
                            while True:
                                try:
                                    targets=parse_ready_pids(ready.read_text(encoding='ascii'),expected_targets)
                                    break
                                except (FileNotFoundError,ValueError):
                                    self.assertLess(time.monotonic(),deadline,'readiness timeout')
                                    time.sleep(.01)
                            tracer_text=pathlib.Path(f'/proc/{p.pid}/task/{p.pid}/children').read_text()
                            tracer_fields=tracer_text.split()
                            self.assertEqual(len(tracer_fields),1,tracer_fields)
                            self.assertTrue(tracer_fields[0].isascii() and tracer_fields[0].isdecimal() and int(tracer_fields[0])>0,tracer_fields)
                            tracer=int(tracer_fields[0])
                            with observe_processes([tracer, *targets]) as handles:
                                p.send_signal(sig);p.wait(timeout=5)
                                deadline=time.monotonic()+3
                                while any(not pidfd_exited(fd) for fd in handles) and time.monotonic()<deadline:time.sleep(.01)
                                self.assertTrue(all(pidfd_exited(fd) for fd in handles),'orphan tracer/target remains alive')
                                self.assertEqual(run(self.store,'record',ready.stem+'-after','--',PROBE,'ok').returncode,0)
                        finally:
                            if p.poll() is None:p.kill();p.wait(timeout=5)


    def test_marker_and_metadata_corruption(self):
        marker=self.directory/'finalized.json';meta=self.directory/'metadata.json'
        original=marker.read_bytes();original_meta=meta.read_bytes()
        for field in ['event_count','events','metadata','invalid_json']:
            with self.subTest(field=field):
                m=json.loads(original)
                if field=='event_count':m[field]+=1
                elif field=='invalid_json':pass
                else:m[field]['bytes']+=1
                marker.write_bytes(b'{broken' if field=='invalid_json' else json.dumps(m).encode())
                p=run(self.store,'diff','base','base','--json')
                self.assertEqual(p.returncode,2)
                self.assertEqual(json.loads(p.stdout)['reliability']['left']['storage'],'corrupt')
                marker.write_bytes(original)
        changed=json.loads(original_meta);changed['command']=['changed']
        meta.write_text(json.dumps(changed))
        try:
            p=run(self.store,'diff','base','base','--json')
            self.assertEqual(p.returncode,2)
            self.assertEqual(json.loads(p.stdout)['reliability']['left']['storage'],'corrupt')
        finally:meta.write_bytes(original_meta)
        for field in ['environment','id']:
            with self.subTest(metadata_field=field):
                changed=json.loads(original_meta)
                if field=='environment':changed.pop(field)
                else:changed[field]='wrong-id'
                meta.write_text(json.dumps(changed))
                try:
                    result=run(self.store,'diff','base','base','--json')
                    self.assertEqual(result.returncode,2)
                    self.assertEqual(json.loads(result.stdout)['reliability']['left']['storage'],'corrupt')
                finally:meta.write_bytes(original_meta)

    def test_diff_exit_semantics(self):
        for target,rc in [('ok',0),('exit143',143)]:
            p=run(self.store,'record',target,'--',PROBE,target);self.assertEqual(p.returncode,rc)
        for other,rc,changed in [('ok',0,False),('exit143',1,True)]:
            data=run(self.store,'diff','base',other,'--json');text=run(self.store,'diff','base',other)
            self.assertEqual(data.returncode,rc);self.assertEqual(text.returncode,rc)
            report=json.loads(data.stdout)
            self.assertEqual(report['observed_differences'],changed)
            self.assertEqual(report['reliability']['left'],{'capture':'complete','storage':'verified'})
            self.assertEqual(report['exit_code'],rc)
            self.assertIn(f'Exit code: {rc}',text.stdout)
        p=run(self.store,'record','failed','--',WORK/'absent');self.assertEqual(p.returncode,125)
        for other in ['base','failed']:
            data=run(self.store,'diff','failed',other,'--json');text=run(self.store,'diff','failed',other)
            self.assertEqual(data.returncode,3);self.assertEqual(text.returncode,3)
            report=json.loads(data.stdout)
            self.assertEqual(report['reliability']['left'],{'capture':'incomplete','storage':'verified'})
            self.assertEqual(report['exit_code'],3)
            self.assertIn('RELIABILITY NOT ESTABLISHED',text.stdout)

    def test_write_failure_never_finalizes(self):
        ready=WORK/'write-failure.ready'
        with (WORK/'write-failure.log').open('w') as log:
            p=subprocess.Popen([str(BIN),'--storage',str(self.store),'record','write-failure','--',str(PROBE),'wait',str(ready)],env=ENV,stdout=log,stderr=log)
            try:
                deadline=time.monotonic()+5
                while not ready.exists():
                    self.assertLess(time.monotonic(),deadline);time.sleep(.01)
                d=next(d for d in (self.store/'runs').iterdir() if d!=self.directory)
                # Deterministic publish failure without guessing the randomized
                # staging name, changing permissions, or filling the user's disk.
                (d/'events.jsonl').mkdir()
                p.terminate();self.assertEqual(p.wait(timeout=5),2)
                self.assertFalse((d/'finalized.json').exists())
                result=run(self.store,'diff','write-failure','base','--json')
                self.assertEqual(result.returncode,3)
                self.assertEqual(json.loads(result.stdout)['reliability']['left']['storage'],'unfinalized')
            finally:
                if p.poll() is None:p.kill();p.wait(timeout=5)

    def test_pty_input_and_interrupt(self):
        import pty,select
        for interrupt in [False,True]:
            with self.subTest(interrupt=interrupt):
                name='tty-interrupt' if interrupt else 'tty-input'
                pid,master=pty.fork()
                if pid==0:
                    os.execvpe(str(BIN),[str(BIN),'--storage',str(self.store),'record',name,'--',str(PROBE),'tty'],ENV)
                fd=os.pidfd_open(pid);output=b'';reaped=False
                try:
                    deadline=time.monotonic()+5
                    while b'TTY_READY' not in output:
                        self.assertLess(time.monotonic(),deadline)
                        if select.select([master],[],[],.05)[0]:output+=os.read(master,65536)
                    os.write(master,b'\x03' if interrupt else b'hello\n')
                    while time.monotonic()<deadline:
                        if select.select([master],[],[],.05)[0]:
                            try:output+=os.read(master,65536)
                            except OSError:pass
                        done,status=os.waitpid(pid,os.WNOHANG)
                        if done:reaped=True;break
                    self.assertTrue(reaped,'PTY command did not exit')
                    self.assertEqual(os.waitstatus_to_exitcode(status),130 if interrupt else 0,output)
                    if not interrupt:self.assertIn(b'TTY_ECHO:hello',output)
                    (WORK/(name+'.txt')).write_bytes(output)
                finally:
                    if not reaped:
                        try:signal.pidfd_send_signal(fd,signal.SIGKILL)
                        except ProcessLookupError:pass
                        os.waitpid(pid,0)
                    os.close(fd);os.close(master)

if __name__=='__main__':
    try:main()
    finally:
        (WORK/'commands.json').write_text(json.dumps(TRANSCRIPT,indent=2))
        print('ARTIFACTS:',WORK)
