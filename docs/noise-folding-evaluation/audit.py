#!/usr/bin/env python3
"""Read-only real-data audit; stdout is a public, path-free summary.
Preserves private full JSON, show/events and raw evidence. Hashes are integrity
fingerprints, not claims of malicious-tamper resistance or durable storage.
"""
import argparse
import hashlib
import json
from pathlib import Path
import view


def digest(p):return hashlib.sha256(p.read_bytes()).hexdigest()


def audit(work):
    # All nine stored records and all original JSON are unchanged by this audit.
    originals=sorted(set(work.glob('*.show.json')) | set(work.glob('*-normal.json')) |
                     set(work.glob('*-abnormal.json')) | {p for p in (work/'records').rglob('*') if p.is_file()})
    before={p:digest(p) for p in originals}
    results=[]
    for result in json.loads((work/'results.json').read_text()):
        case,comparison=result['case'],result['comparison']
        report,rows,path=view.load(work,case,comparison)
        clue_indices=[i for i,c in enumerate(report['changes']) if result['clue'] in c['subject']]
        if comparison=='abnormal':
            assert len(clue_indices)==1
            c=report['changes'][clue_indices[0]]
            assert c['category']=='File access outcome changes'
            assert 'success' in c['left']
            assert ('EACCES' if case=='pip' else 'ENOENT') in c['right']
        measurements={}
        for mode in ['r1','r2','both']:
            v=view.candidate(report,rows,mode)
            expanded=[]; detail_events=0
            for e in v['entries']:
                if e['type']=='change':expanded.append(e['raw_index']);continue
                detail=view.details(v,rows,e['group'])
                expanded.extend(detail['group']['members'])
                for member in detail['changes']:
                    i=member['raw_index'];assert member['change']==report['changes'][i]
                    for side,events in member['events'].items():
                        assert len(events)==len(rows[i]['events'][side])
                        for actual,original in zip(events,rows[i]['events'][side]):
                            assert actual['event']==original['event']
                            assert actual['event_index']==original['event_index']
                            detail_events+=1
            assert sorted(expanded)==list(range(len(report['changes'])))
            assert len(set(expanded))==len(expanded)
            for key in ['reliability','exit_code','observed_differences']:assert v[key]==report[key]
            grouped={i for g in v['groups'] for i in g['members']}
            ranks=[j+1 for j,e in enumerate(v['entries']) if e['type']=='change' and e['raw_index'] in clue_indices]
            folded=bool(grouped.intersection(clue_indices))
            missed=len(ranks)!=len(clue_indices) and not folded
            if comparison=='abnormal':assert ranks and max(ranks)<=5 and not folded and not missed
            measurements[mode]=dict(view_entries=v['view_entries'],groups=len(v['groups']),
                single_entries=sum(e['type']=='change' for e in v['entries']),
                clue_ranks=ranks,clue_in_first_five=bool(ranks and min(ranks)<=5) if clue_indices else None,
                clue_folded=folded,clue_demoted=any(a>b for a,b in zip(ranks,[i+1 for i in clue_indices])),
                clue_missing=missed,detail_event_count=detail_events,
                group_members=[dict(rule=g['rule'],raw_indices_zero_based=g['members'],evidence={
                    k:val for k,val in g['evidence'].items() if k not in ('writer','parent','relative_structure')}) for g in v['groups']])
        results.append(dict(case=case,comparison=comparison,raw_changes=len(rows),
            raw_clue_ranks=[i+1 for i in clue_indices],reliability=report['reliability'],
            diff_exit_code=report['exit_code'],measurements=measurements,
            raw_json_sha256=digest(path),classification='actual capture and view audit',
            target_exit_codes={k:v['exit_code'] for k,v in result['records'].items()}))
    assert all(digest(p)==h for p,h in before.items())
    manifest=json.loads((work/'manifest.json').read_text())
    here=Path(__file__).resolve().parent
    assert digest(here/'EXPECTED_CLUES.md')==manifest['plan_sha256']
    return dict(schema_version=1,plan_sha256=manifest['plan_sha256'],binary_sha256=manifest['binary_sha256'],
        analysis_sha256={p.name:digest(p) for p in [here/'view.py',here/'audit.py',here/'test_view.py',here/'collect.py']},
        originals_unchanged_during_audit=True,original_files_checked=len(originals),
        original_json=[dict(file=p.name,sha256=h) for p,h in before.items() if p.parent==work],results=results)


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('artifacts',type=Path)
    print(json.dumps(audit(p.parse_args().artifacts),ensure_ascii=False,indent=2))
