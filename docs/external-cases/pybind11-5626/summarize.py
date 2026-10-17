#!/usr/bin/env python3
"""Recompute descriptive statistics from the redacted frozen evidence, not runtime rules.
No tool/project names or path-prefix allowlists are used to fold or suppress data.
"""
import csv
import json
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent

def load():
    with (HERE / 'repeat-differences.csv').open(newline='') as f:
        rows = list(csv.DictReader(f))
    for r in rows:
        for k in ['rank', 'event_count']:
            r[k] = int(r[k])
        for k in ['results', 'flags', 'actor_parent_counts']:
            r[k] = json.loads(r[k])
        assert sum(p['count'] for p in r['actor_parent_counts']) == r['event_count']
        assert sorted({p['outcome'] for p in r['actor_parent_counts']}) == r['results']
    return rows

def summarize(rows):
    def freq(values):
        return dict(Counter(values).most_common())
    parents = Counter()
    actors = Counter()
    calls_outcomes = Counter()
    calls_modes = Counter()
    for r in rows:
        for p in r['actor_parent_counts']:
            parents[p['parent']] += p['count']
            actors[p['actor']] += p['count']
            calls_outcomes[p['outcome']] += p['count']
            calls_modes[p['mode']] += p['count']
    groups = []
    for name, count in Counter(r['category'] for r in rows).most_common():
        rs = [r for r in rows if r['category'] == name]
        parent_counts = Counter()
        for r in rs:
            for p in r['actor_parent_counts']:
                parent_counts[p['parent']] += p['count']
        groups.append(dict(category=name, changes=count, file_calls=sum(r['event_count'] for r in rs),
                           sides=freq(r['side'] for r in rs), operations=freq(r['operation'] for r in rs),
                           prefixes=freq(r['prefix'] for r in rs),
                           result_sets=freq('+'.join(r['results']) for r in rs),
                           parent_calls=dict(parent_counts.most_common())))
    with (HERE / 'parent-processes.csv').open(newline='') as f:
        processes = list(csv.DictReader(f))
    assert sum(int(p['file_calls']) for p in processes) == sum(r['event_count'] for r in rows)
    return dict(changes=len(rows), unique_paths=len({r['path'] for r in rows}),
                file_calls=sum(r['event_count'] for r in rows),
                sides=freq(r['side'] for r in rows), operations=freq(r['operation'] for r in rows),
                prefixes=freq(r['prefix'] for r in rows),
                result_sets=freq('+'.join(r['results']) for r in rows),
                calls_per_key=freq(r['event_count'] for r in rows),
                calls_by_mode=dict(calls_modes), calls_by_outcome=dict(calls_outcomes),
                parent_calls=dict(parents.most_common()), actor_calls=dict(actors.most_common()),
                parent_instances=len(processes),
                rows_with_exclusive_create=sum('O_EXCL' in r['flags'] for r in rows),
                groups=groups)

if __name__ == '__main__':
    result = summarize(load())
    assert result['changes'] == 446 and result['file_calls'] == 712 and result['unique_paths'] == 420
    print(json.dumps(result, ensure_ascii=False, indent=2))
