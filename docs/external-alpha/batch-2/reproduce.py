#!/usr/bin/env python3
"""Independent, private-workspace replay. No repository writes or source patches.

Downloads happen only during preparation. Recorded commands need no network.
Existing records/output archives are never overwritten. Python >=3.12, Linux
x86_64, cc/c++/make and an existing RunDelta/strace binary are required.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request

HERE = Path(__file__).resolve().parent
PHASES = ('good', 'repeat', 'bad', 'bad-repeat')
PACKAGES = {'pip-24.2': 'pip==24.2', 'cmake-3.31.6': 'cmake==3.31.6', 'cmake-4.0.3': 'cmake==4.0.3'}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def tree(root):
    return {str(p.relative_to(root)): digest(p) for p in sorted(root.rglob('*')) if p.is_file()}


def write_json(path, data):
    with path.open('x') as stream:
        json.dump(data, stream, indent=2); stream.write('\n')


def setup_env(work):
    return dict(PATH='/usr/bin:/bin', HOME=str(work / 'home'), XDG_CACHE_HOME=str(work / 'cache'),
                TMPDIR=str(work / 'temp'), LC_ALL='C.UTF-8', PYTHONDONTWRITEBYTECODE='1',
                PIP_CONFIG_FILE='/dev/null', PIP_DISABLE_PIP_VERSION_CHECK='1')


def prepare(work):
    for name in ('home', 'cache', 'temp', 'downloads', 'sources', 'tools'):
        (work / name).mkdir()
    env = setup_env(work)
    with (work / 'setup.log').open('x') as log:
        def run(args, extra=None):
            subprocess.run(list(map(str, args)), env=env | (extra or {}), check=True,
                           stdout=log, stderr=subprocess.STDOUT, timeout=600)
        bootstrap = work / 'tools/bootstrap'
        run(['/usr/bin/python3', '-m', 'venv', bootstrap])
        run([bootstrap / 'bin/python', '-m', 'pip', '--isolated', 'install', '--index-url',
             'https://pypi.org/simple', 'uv==0.12.17'])
        run([bootstrap / 'bin/uv', 'python', 'install', '3.12.11', '--no-bin'],
            dict(UV_PYTHON_INSTALL_DIR=str(work / 'pythons'), UV_CACHE_DIR=str(work / 'cache/uv')))
        py = work / 'pythons/cpython-3.12.11-linux-x86_64-gnu/bin/python3.12'
        observed = {}
        expected = json.loads((HERE / 'provenance.json').read_text())['downloads']
        for name, requirement in PACKAGES.items():
            venv = work / 'tools' / name
            run([py, '-m', 'venv', venv])
            dest = work / 'downloads' / name; dest.mkdir()
            run([bootstrap / 'bin/python', '-m', 'pip', '--isolated', 'download', '--index-url',
                 'https://pypi.org/simple', '--no-deps', '--only-binary=:all:', '-d', dest, requirement])
            wheel, = dest.glob('*.whl')
            assert digest(wheel) == expected[name]['sha256'], ('download differs', name)
            observed[name] = dict(filename=wheel.name, sha256=digest(wheel))
            run([venv / 'bin/python', '-m', 'pip', '--isolated', 'install', '--no-index',
                 '--no-deps', '--force-reinstall', wheel])
        spec = expected['expat-sys-2.1.6']
        crate = work / 'downloads/expat-sys-2.1.6.crate'
        urllib.request.urlretrieve(spec['url'], crate)
        assert digest(crate) == spec['sha256'], 'crate differs'
        observed['expat-sys-2.1.6'] = spec
        source = work / 'bracket-lib-376/source'; source.mkdir(parents=True)
        with tarfile.open(crate) as archive:
            archive.extractall(source, filter='data')
        write_json(work / 'downloads.json', observed)


def collect(work, binary, tracer):
    expected = json.loads((HERE / 'provenance.json').read_text())['downloads']
    assert json.loads((work / 'downloads.json').read_text()) == expected
    # Validate prepared downloads too; --prepared-work cannot bypass provenance.
    for name in PACKAGES:
        wheel = work / 'downloads' / name / expected[name]['filename']
        assert digest(wheel) == expected[name]['sha256']
    assert digest(work / 'downloads/expat-sys-2.1.6.crate') == expected['expat-sys-2.1.6']['sha256']
    cmake_bins = {version: work / 'tools' / ('cmake-' + version) / 'lib/python3.12/site-packages/cmake/data/bin'
                  for version in ('3.31.6', '4.0.3')}
    pip = work / 'tools/pip-24.2/bin/pip'
    for p in [binary, tracer, pip, *[d / 'cmake' for d in cmake_bins.values()]]:
        assert p.is_file(), p
    case_hashes = {name: digest(HERE / name / 'CASE.md') for name in ('pip-12996', 'bracket-lib-376')}
    protected_roots = [work / 'tools/pip-24.2', work / 'tools/cmake-3.31.6', work / 'tools/cmake-4.0.3',
                       work / 'bracket-lib-376/source', work / 'pythons']
    before = {str(p.relative_to(work)): tree(p) for p in protected_roots}
    write_json(work / 'preregistered.json', dict(case_sha256=case_hashes, binary_sha256=digest(binary),
                                              tracer_sha256=digest(tracer), source_tools=before))
    all_cases = []
    for name in ('pip-12996', 'bracket-lib-376'):
        case = work / name; case.mkdir(exist_ok=True)
        for folder in ('home/.config/pip', 'cache', 'tmp', 'xdg-empty', 'archives'):
            (case / folder).mkdir(parents=True)
        env = dict(PATH='/usr/bin:/bin', HOME=str(case / 'home'),
                   XDG_CONFIG_HOME=str(case / 'home/.config'), XDG_CONFIG_DIRS=str(case / 'xdg-empty'),
                   XDG_CACHE_HOME=str(case / 'cache'), TMPDIR=str(case / 'tmp'),
                   LANG='C.UTF-8', LC_ALL='C.UTF-8', PYTHONDONTWRITEBYTECODE='1',
                   PIP_DISABLE_PIP_VERSION_CHECK='1', RUNDELTA_STRACE=str(tracer))
        if name == 'pip-12996':
            (case / 'home/.config/pip/pip.conf').write_text('[install]\nconstraint = constraints.txt\nrequire-virtualenv = true\n')
            (case / 'override.conf').write_text('[install]\nonly-binary = numpy,scipy,mpi4py,pandas,h5py,Cython,grpcio\nprefer-binary = true\ndisable-pip-version-check = true\nrequire-virtualenv = true\n')
            command = [str(pip), 'config', 'get', 'install.constraint']
            changed_key = 'PIP_CONFIG_FILE'
            good = env | {changed_key: ''}
            bad = env | {changed_key: str(case / 'override.conf')}
            fixtures = {str(p.relative_to(case)): digest(p) for p in [case / 'override.conf', case / 'home/.config/pip/pip.conf']}
        else:
            command = ['cmake', str(case / 'source/expat-sys-2.1.6/expat'), '-B', str(case / 'build'),
                       '-DBUILD_shared=OFF', '-DBUILD_tools=OFF', '-DBUILD_examples=OFF', '-DBUILD_tests=OFF',
                       '-DCMAKE_INSTALL_PREFIX=' + str(case / 'install'),
                       '-DCMAKE_C_FLAGS= -ffunction-sections -fdata-sections -fPIC -m64 -w', '-DCMAKE_C_COMPILER=/usr/bin/cc',
                       '-DCMAKE_CXX_FLAGS= -ffunction-sections -fdata-sections -fPIC -m64 -w', '-DCMAKE_CXX_COMPILER=/usr/bin/c++',
                       '-DCMAKE_ASM_FLAGS= -ffunction-sections -fdata-sections -fPIC -m64 -w', '-DCMAKE_ASM_COMPILER=/usr/bin/cc',
                       '-DCMAKE_BUILD_TYPE=Debug']
            changed_key = 'PATH'
            good = env | dict(PATH=str(cmake_bins['3.31.6']) + ':/usr/bin:/bin')
            bad = env | dict(PATH=str(cmake_bins['4.0.3']) + ':/usr/bin:/bin')
            fixtures = {}
        assert {k for k in good.keys() | bad.keys() if good.get(k) != bad.get(k)} == {changed_key}
        write_json(case / 'conditions.json', dict(command=command, cwd=str(case), changed_key=changed_key,
                                                good_environment=good, bad_environment=bad, fixture_hashes=fixtures))
        phases = {}
        for phase in PHASES:
            assert digest(HERE / name / 'CASE.md') == case_hashes[name]
            current_env = good if phase in ('good', 'repeat') else bad
            if name == 'bracket-lib-376':
                (case / 'build').mkdir()
            argv = [str(binary), '--storage', str(case / 'records'), 'record', phase, '--', *command]
            with (case / f'{phase}.stdout').open('xb') as out, (case / f'{phase}.stderr').open('xb') as err:
                p = subprocess.run(argv, cwd=case, env=current_env, stdout=out, stderr=err, timeout=180)
            show = subprocess.run([str(binary), '--storage', str(case / 'records'), 'show', phase, '--json'],
                                  env=current_env, capture_output=True, timeout=60)
            assert show.returncode == 0, (name, phase, show.stderr.decode(errors='replace'))
            run = json.loads(show.stdout)
            with (case / f'{phase}.show.json').open('xb') as stream:
                stream.write(show.stdout)
            m = run['metadata']
            assert m['command'] == command and m['cwd'] == str(case)
            phases[phase] = dict(record_exit=p.returncode, id=m['id'], target_exit=m['exit_code'],
                                 collector_exit=m['collector_exit_code'], capture=m['completeness'],
                                 storage=run['storage_integrity']['status'], warnings=m['warnings'],
                                 events=len(run['events']))
            if name == 'bracket-lib-376':
                phases[phase]['makefile_created'] = (case / 'build/Makefile').is_file()
                (case / 'build').rename(case / 'archives' / phase)
            print(name, phase, 'record', p.returncode, 'capture', m['completeness'],
                  'storage', run['storage_integrity']['status'], 'warnings', len(m['warnings']), flush=True)
        diffs = {}
        for label, left, right in [('control', 'good', 'repeat'), ('comparison', 'good', 'bad'), ('bad-control', 'bad', 'bad-repeat')]:
            exits = {}
            for ext, flags in [('json', ['--json']), ('txt', [])]:
                p = subprocess.run([str(binary), '--storage', str(case / 'records'), 'diff', left, right, *flags],
                                   env=good, capture_output=True, timeout=60)
                with (case / f'{label}.{ext}').open('xb') as stream:
                    stream.write(p.stdout)
                with (case / f'{label}.{ext}.stderr').open('xb') as stream:
                    stream.write(p.stderr)
                exits[ext] = p.returncode
            data = json.loads((case / f'{label}.json').read_text())
            assert exits['txt'] == exits['json'] == data['exit_code']
            diffs[label] = dict(exit_code=data['exit_code'], changes=len(data['changes']), reliability=data['reliability'])
            print(name, label, 'diff', data['exit_code'], 'changes', len(data['changes']), flush=True)
        assert all(digest(case / path) == h for path, h in fixtures.items())
        result = dict(case=name, phases=phases, comparisons=diffs, changed_environment_keys=[changed_key],
                      commands_equal=True, cwd_equal=True, fixtures_unchanged=True)
        write_json(case / 'collection.json', result)
        all_cases.append(result)
    assert all(tree(work / name) == entries for name, entries in before.items()), 'source/tool files changed'
    write_json(work / 'collection.json', dict(cases=all_cases, source_tools_unchanged=True,
                                            case_sha256=case_hashes, binary_sha256=digest(binary)))
    print('PASS: commands/CWD equal, exactly one environment key changed, target source/tools unchanged; not a claim of reproduction success')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rundelta', type=Path, required=True)
    parser.add_argument('--strace', type=Path, required=True)
    parser.add_argument('--prepared-work', type=Path, help='Reuse only an unrecorded workspace with the exact prepared downloads; never reuse records')
    args = parser.parse_args()
    work = args.prepared_work.resolve() if args.prepared_work else Path(tempfile.mkdtemp(prefix='rundelta-alpha-batch2-replay-'))
    print('PRIVATE WORKSPACE:', work, flush=True)
    if not args.prepared_work:
        prepare(work)
    collect(work, args.rundelta.resolve(strict=True), args.strace.resolve(strict=True))
