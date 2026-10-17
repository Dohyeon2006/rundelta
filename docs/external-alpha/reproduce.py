#!/usr/bin/env python3
"""Run public fixtures outside the checkout; write private raw records only there.
Requires Linux, Python 3.14 with venv, git, ar, gcc/g++, make, and Rust on PATH.
No source patching, commits, uploads, ranking changes or system package installs.
"""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rundelta', type=Path, required=True)
    parser.add_argument('--strace', type=Path, required=True)
    parser.add_argument('--case', choices=['pybind', 'meson', 'spades', 'all'], default='all')
    args = parser.parse_args()
    rd, tracer = args.rundelta.resolve(strict=True), args.strace.resolve(strict=True)
    public = Path(__file__).resolve().parent
    provenance = json.loads((public / 'provenance.json').read_text())
    work = Path(tempfile.mkdtemp(prefix='rundelta-alpha-replay-'))
    print('Private workspace:', work, flush=True)
    rust = Path(subprocess.check_output(['rustup', 'which', 'cargo'], text=True).strip()).parent
    for directory in ['home', 'cache', 'temp', 'cargo-home']:
        (work / directory).mkdir()
    env = dict(PATH=f'{work}/tools/bin:{rust}:/usr/bin:/bin', HOME=str(work / 'home'),
               LANG='C.UTF-8', LC_ALL='C.UTF-8', PYTHONDONTWRITEBYTECODE='1',
               XDG_CACHE_HOME=str(work / 'cache'), TMPDIR=str(work / 'temp'),
               CARGO_HOME=str(work / 'cargo-home'), RUSTC=str(rust / 'rustc'),
               RUNDELTA_STRACE=str(tracer))

    def run(command, **kwargs):
        return subprocess.run(list(map(str, command)), env=env, check=True, **kwargs)

    def download(name):
        source = provenance['downloads'][name]
        target = work / name
        urllib.request.urlretrieve(source['url'], target)
        assert hashlib.sha256(target.read_bytes()).hexdigest() == source['sha256'], name
        return target

    def clone(repo, name):
        target = work / name / 'upstream'
        run(['git', 'init', target], stdout=subprocess.DEVNULL)
        run(['git', '-C', target, 'fetch', '--depth=1', f'https://github.com/{repo}.git',
             provenance['source_commits'][name]])
        run(['git', '-C', target, 'checkout', '--detach', 'FETCH_HEAD'])
        return target

    def preregister(name):
        mapping = dict(pybind='pybind11-3445', meson='meson-16136', spades='spades-1339')
        directory = work / name
        directory.mkdir()
        for source in (public / mapping[name]).glob('*.md'):
            if source.name in ['CASE.md', 'RETRY.md', 'RETRY-EXACT.md']:
                shutil.copyfile(source, directory / source.name)
        return directory

    def record(directory, name, command, cwd, extra=None, selected=()):
        record_env = env | (extra or {})
        argv = [str(rd), '--storage', str(directory / 'records'), 'record', name]
        for key in selected:
            argv += ['--env', key]
        argv += ['--'] + list(map(str, command))
        with (directory / f'{name}.stdout').open('wb') as out, (directory / f'{name}.stderr').open('wb') as err:
            result = subprocess.run(argv, cwd=cwd, env=record_env, stdout=out, stderr=err)
        (directory / f'{name}.rc').write_text(str(result.returncode))
        with (directory / f'{name}.show.json').open('wb') as out:
            subprocess.run([str(rd), '--storage', str(directory / 'records'), 'show', name, '--json'],
                           env=env, stdout=out, check=True)
        print(directory.name, name, 'record exit', result.returncode, flush=True)

    def compare(directory, left='good', right='bad', label='diff'):
        for extension, options in [('json', ['--json']), ('txt', [])]:
            with (directory / f'{label}.{extension}').open('wb') as out:
                result = subprocess.run([str(rd), '--storage', str(directory / 'records'),
                                         'diff', left, right] + options, env=env, stdout=out)
            (directory / f'{label}.{extension}.rc').write_text(str(result.returncode))
        report = json.loads((directory / f'{label}.json').read_text())
        print(directory.name, label, 'diff exit', report['exit_code'], 'changes', len(report['changes']),
              'reliability', report['reliability'], flush=True)

    run(['/usr/bin/python3', '-m', 'venv', work / 'tools'])
    run([work / 'tools/bin/pip', '--isolated', 'install', '--index-url', 'https://pypi.org/simple',
         'cmake==3.21.1.post1', 'ninja==1.13.2', 'uv==0.12.17'])
    uv_env = env | dict(UV_PYTHON_INSTALL_DIR=str(work / 'pythons'), UV_CACHE_DIR=str(work / 'cache'))
    subprocess.run([str(work / 'tools/bin/uv'), 'python', 'install', '3.9.25', '3.10.12', '3.10.19', '--no-bin'],
                   env=uv_env, check=True)
    if args.case in ['pybind', 'all']:
        d = preregister('pybind')
        clone('pybind/pybind11', 'pybind')
        run([work / 'pythons/cpython-3.9.25-linux-x86_64-gnu/bin/python3.9', '-m', 'venv', d / 'venv'])
        run([d / 'venv/bin/python', '-m', 'pip', '--isolated', 'install', '--index-url',
             'https://pypi.org/simple', 'pybind11==2.8.1'])
        source = d / 'consumer'
        source.mkdir()
        (source / 'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.21.1)\nproject(Reproduce)\nfind_package(pybind11 REQUIRED)\n')
        prefix = subprocess.check_output([str(d / 'venv/bin/python'), '-m', 'pybind11', '--cmakedir'],
                                         env=env, text=True).strip()
        build = d / 'build'
        build.mkdir()
        command = [work / 'tools/bin/cmake', '-S', source, '-B', build]
        extra = dict(PATH=f'{d}/venv/bin:' + env['PATH'], VIRTUAL_ENV=str(d / 'venv'))
        record(d, 'good', command, d, extra | dict(CMAKE_PREFIX_PATH=prefix), ['CMAKE_PREFIX_PATH'])
        build.rename(d / 'good-build')
        build.mkdir()
        record(d, 'bad', command, d, extra, ['CMAKE_PREFIX_PATH'])
        compare(d)
    if args.case in ['meson', 'all']:
        d = preregister('meson')
        upstream = clone('mesonbuild/meson', 'meson')
        with zipfile.ZipFile(download('meson.zip')) as archive:
            for entry in archive.namelist():
                assert not Path(entry).is_absolute() and '..' not in Path(entry).parts
            archive.extractall(d / 'fixture')
        source = d / 'fixture/meson-cargo-path-dep-reproducer'
        record(d, 'good', [rust / 'cargo', 'build', '--offline', '--locked', '--manifest-path',
                          source / 'Cargo.toml', '--target-dir', d / 'cargo-output'], d)
        record(d, 'bad', ['/usr/bin/python3', '-B', upstream / 'meson.py', 'setup',
                         d / 'meson-output', source, '--wrap-mode=nodownload'], d)
        compare(d)
    if args.case in ['spades', 'all']:
        d = preregister('spades')
        clone('ablab/spades', 'spades')
        archive_path = download('spades.tar.gz')
        for directory in ['install', 'install with spaces']:
            with tarfile.open(archive_path) as archive:
                archive.extractall(d / directory, filter='data')
        shared_cwd = d / 'run'
        shared_cwd.mkdir()
        # Preserve the original failed bootstrap pair and successful alternative pair.
        for version, suffix in [('3.10.12', ''), ('3.10.19', '-v2')]:
            for name, installation in [('good', 'install'), ('bad', 'install with spaces')]:
                record(d, name + suffix, [d / installation / 'SPAdes-4.0.0-Linux/bin/spades.py', '--test', '--careful'],
                       shared_cwd, dict(PATH=f'{work}/pythons/cpython-{version}-linux-x86_64-gnu/bin:' + env['PATH']))
                output = shared_cwd / 'spades_test'
                if output.exists():
                    output.rename(d / (name + suffix + '-output'))
            compare(d, 'good' + suffix, 'bad' + suffix, 'diff' + suffix)
        deb = download('libcrypt.deb')
        data = subprocess.check_output(['ar', 'p', str(deb), 'data.tar.xz'])
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            archive.extractall(work / 'compat', filter='data')
        extra = dict(PATH=f'{work}/pythons/cpython-3.10.12-linux-x86_64-gnu/bin:' + env['PATH'],
                     LD_LIBRARY_PATH=str(work / 'compat/lib/x86_64-linux-gnu'))
        for name, installation, runtime in [('good-v3', 'install', 'runtime/bin'),
                                            ('bad-v3', 'install with spaces', 'runtime with spaces/bin')]:
            cwd = d / runtime
            cwd.mkdir(parents=True)
            record(d, name, [d / installation / 'SPAdes-4.0.0-Linux/bin/spades.py', '--test', '--careful'], cwd, extra)
        compare(d, 'good-v3', 'bad-v3', 'diff-v3')
    print('Keep raw output private. Do not copy this workspace into a repository.')


if __name__ == '__main__':
    main()
