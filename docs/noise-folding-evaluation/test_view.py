"""Independent candidate-view tests; synthetic inputs are NOT capture evidence."""
import copy
import unittest
import view


def row(i, side, path, count=1):
    return dict(index=i, eligible=True, side=side, path=path, operation='openat',
                count=count, lineage=(1 if side=='left' else 9, 2 if side=='left' else 8,
                                      '/tools/producer','/tools/parent'), change={}, events={})


def family():
    return [row(i, 'left' if i<2 else 'right', '/arbitrary/item.'+token)
            for i,token in enumerate(['aaaa','bbbb','cccc','dddd'])]


class Rules(unittest.TestCase):
    def test_recurring_and_pid_independence(self):
        self.assertEqual(view.recurring(family())[0]['members'],[0,1,2,3])

    def test_counts_parent_actor_and_operation_must_agree(self):
        for key,value in [('count',7),('lineage',(4,5,'/tools/other','/tools/parent')),
                          ('operation','access')]:
            rows=family(); rows[0][key]=value
            self.assertEqual(view.recurring(rows),[],key)
        rows=family();rows[0]['lineage']=(1,99,'/tools/producer','/tools/parent')
        self.assertEqual(view.recurring(rows),[])

    def test_no_repetition_no_group(self):
        self.assertEqual(view.recurring(family()[::2]),[])

    def test_namespace_and_unmatched_member(self):
        rows=[row(i,side,f'/arbitrary/{slot}/{name}') for i,(side,slot,name) in enumerate(
            [(side,slot,name) for side,slot in [('left','aaaa'),('right','bbbb')]
             for name in ['one','two','three']])]
        rows.append(row(6,'right','/arbitrary/bbbb/four'))
        g=view.namespaces(rows)[0]
        self.assertEqual(g['members'],list(range(6)))
        self.assertEqual(g['evidence']['unmatched_eligible_keys'],1)
        rows[0]['count']=2
        self.assertEqual(view.namespaces(rows),[])

    def test_error_and_outcome_changes_protected(self):
        for left,right in [([],['EACCES']),(['success'],['ENOENT']),([],['success','ENOENT'])]:
            report={'changes':[{'category':'File access outcome changes','subject':'/x (openat)',
                                'left':left,'right':right}]}
            rows=view.prepare(report,{'events':[]},{'events':[]},{'left':[],'right':[]})
            self.assertFalse(rows[0]['eligible'])

    def test_flags_and_unknown_actor_gate(self):
        event=dict(pid=1,kind='file',path='/x',operation='openat',outcome='success',
                   evidence=dict(line=1,end_line=1))
        report={'changes':[dict(category='New file access',subject='/x (openat)',left=[],right=['success'])]}
        rows=view.prepare(report,{'events':[]},{'events':[event]},
                          {'left':[],'right':['openat(AT_FDCWD, "/x", O_CREAT|O_TRUNC) = 3']})
        self.assertFalse(rows[0]['eligible'])
        ex=dict(pid=1,kind='exec',path='/producer',outcome='success',evidence=dict(line=1,end_line=1))
        event['evidence']=dict(line=2,end_line=2)
        for flags,expected in [('O_RDONLY',False),('O_CREAT|O_TRUNC',True),('O_CREAT|O_EXCL',True)]:
            rows=view.prepare(report,{'events':[]},{'events':[ex,event]},
                              {'left':[],'right':['execve(...) = 0',f'openat(..., {flags}) = 3']})
            self.assertEqual(rows[0]['eligible'],expected)
            if expected:self.assertEqual(rows[0]['lineage'][3],'<trace boundary>')

    def test_content_counterexample_is_not_proof_of_equivalence(self):
        # Same observed behavior, different important content: both are grouped.
        # This PASS demonstrates a known risk, NOT safety or irrelevance.
        a=family(); b=copy.deepcopy(a)
        for r in b:r['unobserved_content']='incorrect generated configuration'
        self.assertEqual(view.recurring(a),view.recurring(b))
        self.assertIn('unverified',view.recurring(b)[0]['equivalence'])

    def test_coverage_and_quality_preserved(self):
        rows=family();rows.append(dict(index=4,eligible=False))
        report=dict(observed_differences=True,reliability={'capture':'incomplete'},exit_code=3)
        v=view.candidate(report,rows)
        self.assertEqual(v['view_entries'],2)
        self.assertEqual(v['entries'][-1]['raw_index'],4)
        self.assertEqual(v['reliability'],report['reliability'])
        self.assertEqual(v['exit_code'],3)


if __name__=='__main__':unittest.main()
