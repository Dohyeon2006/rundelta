"""GDB-only observer. Breakpoints bracket ORIGINAL product code; no injected loop."""
import gdb
import json
import os
import time

DATA={'hits':{}}
START=None

def memory():
    pid=gdb.selected_inferior().pid
    with open('/proc/%s/status' % pid) as f:
        values={line.split(':',1)[0]:line.split(':',1)[1].strip() for line in f if ':' in line}
    return {key:int(values[key].split()[0]) for key in ['VmRSS','VmHWM']}

class Mark(gdb.Breakpoint):
    def __init__(self,label,line):
        super().__init__('src/capture/mod.rs:%d' % line,internal=True)
        self.label=label
    def stop(self):
        global START
        now=time.perf_counter_ns()
        DATA['hits'][self.label]=DATA['hits'].get(self.label,0)+1
        DATA[self.label+'_frame']=gdb.newest_frame().name()
        if self.label=='start':
            DATA['start_memory_kib']=memory()
            START=time.perf_counter_ns()
        elif self.label=='end':
            DATA['completion_wall_ms']=(now-START)/1e6 if START else None
            DATA['end_memory_kib']=memory()
        else:
            DATA['final_memory_kib']=memory()
        self.enabled=False
        return False

Mark('start',198)
Mark('end',209)
Mark('final',258)

def exited(event):
    DATA['exit_code']=getattr(event,'exit_code',None)
    with open(os.environ['A3_PROBE_OUTPUT'],'w') as f:json.dump(DATA,f,indent=2)
gdb.events.exited.connect(exited)
