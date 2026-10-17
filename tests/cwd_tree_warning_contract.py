"""Independent cwd-tree warning contract; historical five-case v2 stays frozen."""
from collections import Counter
import re
from field_warning_contract import CWD_PREFIX

FS_CONTEXT_PREFIX = re.compile(r'^raw/trace\.log:([1-9][0-9]*): fs\.context:')


def audit_directory_warnings(warnings, raw_lines):
    """Require the cwd-tree categories and their exact syscall evidence."""
    actual = Counter()
    for warning in warnings:
        assert isinstance(warning, str), warning
        unresolved = CWD_PREFIX.match(warning)
        context = FS_CONTEXT_PREFIX.match(warning)
        match = unresolved or context
        assert match is not None, warning
        line = int(match[1])
        assert 1 <= line <= len(raw_lines), warning
        raw = re.sub(r'\\x([0-9a-fA-F]{2})', lambda m: chr(int(m[1], 16)), raw_lines[line-1])
        operands = ('does-not-exist',) if unresolved else ('parent', '../child')
        hits = [operand for operand in operands if re.search(
            r'chdir\("' + re.escape(operand) + r'"\)\s+=\s+' +
            (r'-1\b' if unresolved else r'0(?:\s|$)'), raw)]
        assert len(hits) == 1, (warning, raw)
        actual[('path context unresolved' if unresolved else 'fs.context', hits[0])] += 1
    expected = Counter({('fs.context', 'parent'): 1, ('fs.context', '../child'): 1,
                        ('path context unresolved', 'does-not-exist'): 1})
    assert actual == expected, (actual, expected)
