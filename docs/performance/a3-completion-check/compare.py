#!/usr/bin/env python3
"""Reuse frozen measure.py input matrix/validation, remapping source breakpoints.
Example: python3 -B compare.py --source-root . --binary target/debug/rundelta
 --profile dev-after --output /tmp/after.json
Release requires CARGO_PROFILE_RELEASE_DEBUG=2; optimizations remain enabled.
No source instrumentation or timing loop substitutes for capture::record.
"""
import argparse
import hashlib
import re
import subprocess
import json
from pathlib import Path
import shutil
import tempfile

HERE = Path(__file__).resolve().parent

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--source-root', type=Path, required=True)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--profile', required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--machine-call', action='store_true', help='bracket the unique aggregate/collect call in capture::record (release)')
    args = p.parse_args()
    root = args.source_root.resolve()
    lines = (root/'src/capture/mod.rs').read_text().splitlines()
    def line(fragment):
        hits = [i+1 for i,s in enumerate(lines) if fragment in s]
        assert len(hits)==1, (fragment,hits)
        return hits[0]
    optimized = any('let completion = Completion::from_events' in s for s in lines)
    start = line('let completion = Completion::from_events') if optimized else line('let missing_exits:')
    end = line('let root_exit = completion.root_exit') if optimized else line('if !missing_exits.is_empty()')
    final = next(i+1 for i,s in enumerate(lines) if s.strip() == 'eprintln!(')
    with tempfile.TemporaryDirectory(prefix='a3-harness-') as temp:
        harness = Path(temp)
        for name in ['PLAN.md','measure.py','probe_gdb.py','synthetic_backend.py']:
            shutil.copy2(HERE/name, harness/name)
        probe = (harness/'probe_gdb.py').read_text()
        for name,n,old in [('start',start,198),('end',end,209),('final',final,258)]:
            probe = probe.replace(f"Mark('{name}',{old})", f"Mark('{name}',{n})")
        bounds = None
        if args.machine_call:
            symbols = subprocess.check_output(['nm',str(args.binary.resolve())],text=True)
            symbols = [row.split()[-1] for row in symbols.splitlines() if row.split()[-1].endswith('7capture6record')]
            assert len(symbols)==1, symbols
            symbol = symbols[0]
            assembly = subprocess.check_output(['gdb','-q','-batch',str(args.binary.resolve()),
                '-ex','set language c','-ex','disassemble /r '+symbol],text=True,stderr=subprocess.DEVNULL)
            instructions = [row for row in assembly.splitlines() if re.search(r'<\+\d+>:',row)]
            calls = [i for i,row in enumerate(instructions) if 'call' in row and
                     ('10Completion11from_events' in row if optimized else '8BTreeSet' in row and '9from_iter' in row)]
            assert len(calls)==1, calls
            i = calls[0]
            offsets = [int(re.search(r'<\+(\d+)>:',instructions[j])[1]) for j in [i,i+1]]
            bounds = dict(symbol=symbol, start_offset=offsets[0], end_offset=offsets[1],
                          call_target=instructions[i].split('call',1)[1].strip().split('<',1)[1].rstrip('>'))
            probe = "gdb_placeholder = None\n" + probe
            probe = probe.replace('import gdb', "import gdb\ngdb.execute('set language c')")
            probe = probe.replace("super().__init__('src/capture/mod.rs:%d' % line,internal=True)",
                                  "super().__init__(line if isinstance(line,str) else 'src/capture/mod.rs:%d' % line,internal=True)")
            for label, oldline, offset in [('start',start,offsets[0]),('end',end,offsets[1])]:
                probe = probe.replace(f"Mark('{label}',{oldline})", f"Mark('{label}',{('*'+symbol+'+'+str(offset))!r})")
        (harness/'probe_gdb.py').write_text(probe)
        code = (HERE/'measure.py').read_text()
        code = code.replace('ROOT=HERE.parents[2]', 'ROOT=Path('+repr(str(root))+')')
        code = code.replace("cwd=ROOT,text=True", 'cwd=Path('+repr(str(HERE.parents[2]))+'),text=True')
        code = code.replace("    assert 'let missing_exits:' in lines[197] and 'if !missing_exits.is_empty()' in lines[208]\n", '')
        code = code.replace("    assert 'eprintln!' in lines[257], 'review breakpoint mapping before running on changed source'\n", '')
        code = code.replace("profile='existing cargo dev binary; opt-level=0; debug symbols'", 'profile='+repr(args.profile))
        # Persist the same numeric result plus explicit interval provenance.
        code = code.replace("        print('RESULT_JSON:',work/'results.json',flush=True)",
            "        result['interval']="+repr(dict(start_line=start,end_line=end,final_line=final,scope='aggregate root and all PID exits' if optimized else 'missing exits only', machine_call=bounds, adapter_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()))+"\n        Path("+repr(str(args.output.resolve()))+").write_text(json.dumps(result,indent=2)+'\\n')\n        print('RESULT_JSON:',work/'results.json',flush=True)")
        import sys
        sys.argv = [str(harness/'measure.py'),'--binary',str(args.binary.resolve()),'--repeats','3']
        exec(compile(code,str(harness/'measure.py'),'exec'), {'__name__':'__main__','__file__':str(harness/'measure.py')})

if __name__ == '__main__': main()
