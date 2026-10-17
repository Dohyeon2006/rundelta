#!/usr/bin/env python3
"""Experimental view only. Never rewrites records or calls product folding/ranking.
Rules use paired path structure, observed write intent, process-instance and
operation/outcome/count evidence. No tool names, extensions or project roots
are noise predicates. All groups are explicitly unverified equivalences.
"""
import argparse
from collections import Counter,defaultdict
import hashlib
import json
from pathlib import Path
import re


def tokens(path):
    return re.split(r'([/._-])',path)

def matchable(token):
    # A structural slot proposal, NOT a noise classifier. Behavioral gates follow.
    return len(token)>=4 and token.isalnum()

def features(run, raw_lines):
    identities={};parents={};result=defaultdict(list)
    for index,e in sorted(enumerate(run['events']),key=lambda x:(x[1]['evidence']['line'],x[1]['evidence']['end_line'])):
        pid=e['pid']
        if e['kind']=='exec' and e['outcome']=='success': identities[pid]=e['path']
        if e['kind']=='spawn' and e['child_pid']:
            parents[e['child_pid']]=pid;identities[e['child_pid']]=identities.get(pid)
        if e['kind']!='file':continue
        a,b=e['evidence']['line'],e['evidence']['end_line']
        text=raw_lines[a-1]+(' '+raw_lines[b-1] if a!=b else '')
        flags=set(re.findall(r'\bO_[A-Z0-9_]+\b',text))
        parent=parents.get(pid)
        result[(e['path'],e['operation'])].append(dict(event_index=index,event=e,
            actor=identities.get(pid),parent=identities.get(parent) if parent else '<trace boundary>',
            actor_instance=pid,parent_instance=parent or '<trace boundary>',flags=sorted(flags)))
    return result

def prepare(report, left, right, lines):
    lookup={side:features(run,lines[side]) for side,run in [('left',left),('right',right)]}
    rows=[]
    for i,c in enumerate(report['changes']):
        row=dict(index=i,change=c,eligible=False,events={})
        if c['category'] in ('New file access','Removed file access','File access outcome changes'):
            path,op=c['subject'].rsplit(' (',1);op=op[:-1]
            row.update(path=path,operation=op)
            for side in ['left','right']:row['events'][side]=lookup[side].get((path,op),[])
            side='right' if not c['left'] else 'left' if not c['right'] else None
            if side and (c['right'] or c['left'])==['success']:
                es=row['events'][side]
                generated=any(e['event']['outcome']=='success' and 'O_CREAT' in e['flags'] and
                              ('O_EXCL' in e['flags'] or 'O_TRUNC' in e['flags']) for e in es)
                lineage={(e['actor_instance'],e['parent_instance'],e['actor'],e['parent']) for e in es}
                if generated and len(lineage)==1 and next(iter(lineage))[2] is not None:
                    row.update(eligible=True,side=side,lineage=next(iter(lineage)),count=len(es))
        rows.append(row)
    return rows

def lineage_gate(left,right):
    # Within a run, require the SAME actual producer and immediate-parent instance.
    # Across runs compare executable identities only: never compare numeric PIDs.
    if len({r['lineage'] for r in left})!=1 or len({r['lineage'] for r in right})!=1:return False
    return left[0]['lineage'][2:]==right[0]['lineage'][2:]

def proposal(rule, members, pattern, evidence):
    return dict(rule=rule,members=sorted(r['index'] for r in members),pattern=pattern,
                evidence=evidence,equivalence='unverified; grouping is not proof of identical content or task',
                includes_errors=False)

def recurring(rows):
    buckets=defaultdict(list)
    for r in rows:
        if not r['eligible']:continue
        ts=tokens(r['path'])
        for i,t in enumerate(ts):
            if matchable(t):
                pattern=ts.copy();pattern[i]='{slot}'
                buckets[(''.join(pattern),r['operation'])].append((r,t))
    out=[]
    for (pattern,op),items in buckets.items():
        left=[r for r,t in items if r['side']=='left'];right=[r for r,t in items if r['side']=='right']
        if min(len(left),len(right))<2 or not lineage_gate(left,right):continue
        values={s:{t for r,t in items if r['side']==s} for s in ['left','right']}
        if values['left'] & values['right']:continue
        if sorted(r['count'] for r in left)!=sorted(r['count'] for r in right):continue
        out.append(proposal('R1-recurring-generated-family',left+right,pattern,dict(operation=op,
            left_instances=len(left),right_instances=len(right),calls_per_path=sorted(r['count'] for r in left),
            writer=left[0]['lineage'][2],parent=left[0]['lineage'][3],same_process_within_each_run=True,
            write_witness='successful O_CREAT with O_EXCL or O_TRUNC on every member')))
    return out

def namespaces(rows):
    buckets=defaultdict(list)
    for r in rows:
        if not r['eligible']:continue
        ts=tokens(r['path'])
        for i,t in enumerate(ts):
            if not matchable(t) or '/' not in ts[i+1:]:continue
            end=ts.index('/',i+1)
            prefix=ts[:end];pattern=prefix.copy();pattern[i]='{slot}'
            rel=''.join(ts[end+1:]);literal=''.join(prefix)
            buckets[''.join(pattern)].append((r,literal,rel))
    out=[]
    for pattern,items in buckets.items():
        prefixes={side:{prefix for r,prefix,rel in items if r['side']==side} for side in ['left','right']}
        if any(len(v)!=1 for v in prefixes.values()) or prefixes['left']&prefixes['right']:continue
        sides={s:defaultdict(list) for s in ['left','right']}
        for r,prefix,rel in items:sides[r['side']][(rel,r['operation'],r['count'])].append(r)
        shared=sides['left'].keys() & sides['right'].keys()
        shared={key for key in shared if len(sides['left'][key])==len(sides['right'][key])==1}
        if len(shared)<3:continue
        left=[sides['left'][k][0] for k in sorted(shared)];right=[sides['right'][k][0] for k in sorted(shared)]
        if not lineage_gate(left,right):continue
        members=left+right
        out.append(proposal('R2-paired-generated-subtree',members,pattern,dict(relative_keys=len(shared),
            relative_structure=[dict(path=k[0],operation=k[1],calls=k[2]) for k in sorted(shared)],
            unmatched_eligible_keys=len(items)-len(members),writer=left[0]['lineage'][2],
            parent=left[0]['lineage'][3],same_process_within_each_run=True,
            write_witness='successful O_CREAT with O_EXCL or O_TRUNC on every grouped member')))
    return out

def candidate(report,rows,mode='both'):
    proposals=(recurring(rows) if mode in ('both','r1') else [])+(namespaces(rows) if mode in ('both','r2') else [])
    # Deterministic, largest disjoint proposal first; never silently trim overlaps.
    proposals.sort(key=lambda p:(-len(p['members']),p['rule'],p['pattern']))
    used=set();groups=[]
    for p in proposals:
        if not used.intersection(p['members']):
            p=dict(p,id='G'+str(len(groups)+1));groups.append(p);used.update(p['members'])
    entries=[dict(type='change',raw_index=i) for i in range(len(rows)) if i not in used]
    entries += [dict(type='group',group=g['id'],raw_index=min(g['members'])) for g in groups]
    entries.sort(key=lambda e:e['raw_index'])
    assert sorted([e['raw_index'] for e in entries if e['type']=='change']+[i for g in groups for i in g['members']])==list(range(len(rows)))
    return dict(schema_version=1,experimental=True,mode=mode,raw_changes=len(rows),view_entries=len(entries),
                groups=groups,entries=entries,observed_differences=report['observed_differences'],
                reliability=report['reliability'],exit_code=report['exit_code'])

def load(work,case,comparison):
    right='repeat' if comparison=='normal' else 'bad'
    report_path=work/f'{case}-{comparison}.json'
    report=json.loads(report_path.read_text())
    runs={s:json.loads((work/f'{case}-{p}.show.json').read_text()) for s,p in [('left','good'),('right',right)]}
    lines={s:(work/'records/runs'/r['metadata']['id']/'raw/trace.log').read_text().splitlines() for s,r in runs.items()}
    return report,prepare(report,runs['left'],runs['right'],lines),report_path

def details(view,rows,group_id):
    group=next(g for g in view['groups'] if g['id']==group_id)
    return dict(group=group,changes=[dict(raw_index=i,change=rows[i]['change'],
        events={s:[dict(event_index=e['event_index'],event=e['event'],actor=e['actor'],parent=e['parent'],flags=e['flags'])
                   for e in es] for s,es in rows[i]['events'].items()}) for i in group['members']])

if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('work',type=Path);parser.add_argument('case');parser.add_argument('comparison',choices=['normal','abnormal'])
    parser.add_argument('--mode',choices=['r1','r2','both'],default='both');parser.add_argument('--details',metavar='GROUP')
    args=parser.parse_args()
    report,rows,path=load(args.work,args.case,args.comparison)
    view=candidate(report,rows,args.mode)
    view['raw_json_sha256']=hashlib.sha256(path.read_bytes()).hexdigest()
    print(json.dumps(details(view,rows,args.details) if args.details else view,ensure_ascii=False,indent=2))
