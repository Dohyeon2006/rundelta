"""Predeclared contracts for *new* runs of the unchanged five-case workload.

This is audit data, not product filtering. No values are learned from results.json.
The historical v2 contract remains separate, including its old Cargo warning.
"""
from collections import Counter

CASES = ('python-venv', 'pkg-config', 'git-identity', 'gcc-header', 'cargo-linker')
BAD_CODES = dict(zip(CASES, (1, 1, 128, 1, 101)))
FIELDS = ('type', 'kind', 'operation', 'operand', 'outcome', 'actor', 'mode')


def access(operand, outcome, mode, actor='root'):
    return ('cwd.relative_access', 'file', 'access', operand, outcome, actor, mode)


def warnings_for(name, phase):
    assert name in CASES and phase in ('good', 'repeat', 'bad')
    if name == 'pkg-config':
        return Counter({access('~/.config/rpm/rpmrc', 'ENOENT', 'R_OK', 'rpm_eval_child'): 1})
    if name == 'git-identity':
        expected = Counter({
            access('.git/config', 'success', 'R_OK'): 2,
            access('.git/objects/4b/825dc642cb6eb9a060e54bf8d69288fbee4904', 'ENOENT', 'F_OK'): 1,
            access('.git/hooks/post-index-change', 'ENOENT', 'X_OK'): 1,
        })
        if phase != 'bad':
            expected[access('.git/config', 'success', 'R_OK', 'maintenance_child')] = 2
            expected[access('.git/objects/79/951067511f83f81e9343de2c079f48c7b5b289', 'ENOENT', 'F_OK')] = 1
            for hook in ('pre-commit', 'prepare-commit-msg', 'commit-msg', 'post-commit'):
                expected[access('.git/hooks/' + hook, 'ENOENT', 'X_OK')] = 1
            expected[access('.git/hooks/reference-transaction', 'ENOENT', 'X_OK')] = 7
        return expected
    # Fresh FsContext captures must have ZERO Cargo warnings. An old global
    # CLONE_FS diagnostic, or any newly introduced uncertainty, fails this gate.
    return Counter()


def describe(counter):
    return [dict(zip(FIELDS, key), count=count) for key, count in sorted(counter.items())]


def clue_for(name, work):
    category, kind, operation, suffix, left, right, actor, exact_rank = {
        'python-venv': ('Executable changes', 'exec', 'execve',
                        'python-venv/isolated-env/bin/python3', [], ['success'], 'root', None),
        'pkg-config': ('New file access', 'file', 'openat',
                       'pkg-config/empty-pc', [], ['success'], 'root', None),
        'git-identity': ('File access outcome changes', 'file', 'access',
                         'git-identity/home/.gitconfig', ['success'], ['ENOENT'], 'root', None),
        'gcc-header': ('File access outcome changes', 'file', 'openat',
                       'gcc-header/strace-source/src/defs.h', ['success'], ['ENOENT'], 'compiler_child', 2),
        'cargo-linker': ('Execution attempt changes', 'exec', 'execve',
                         'cargo-linker/missing-linker', [], ['ENOENT'], 'rustc_linker_child', 2),
    }[name]
    return dict(category=category, kind=kind, operation=operation, path=str(work / suffix),
                path_semantics='observed_absolute', left=left, right=right,
                actor=actor, max_rank=5, exact_rank=exact_rank,
                actors_left=({'root': 3, 'maintenance_child': 3} if name == 'git-identity' else {actor: 1} if left else {}),
                actors_right={actor: 3 if name == 'git-identity' else 1})


def audit_command(name, command, work, root):
    expected = {
        'python-venv': ['python3', '/usr/share/doc/python3-pycparser/examples/c-to-c.py',
                        '/usr/share/doc/python3-pycparser/examples/c_files/basic.c'],
        'pkg-config': ['pkg-config', '--cflags', '--libs', 'libcrypt'],
        'git-identity': ['git', '-c', 'user.useConfigOnly=true', 'commit', '--allow-empty', '-m', 'Build environment check'],
        'cargo-linker': ['cargo', 'build', '--offline', '--jobs', '1', '--manifest-path',
                         str(root / 'Cargo.toml'), '--target-dir', str(work / 'cargo-linker/build')],
    }
    if name == 'gcc-header':
        # The frozen generator obtains configured GCC options from make -n.
        assert command[0] == 'gcc' and '-DHAVE_CONFIG_H' in command
        assert command[-4:] == ['-c', '-o', 'strace-strace.o', 'strace.c']
    else:
        assert command == expected[name], ('registered command changed', name, command)
