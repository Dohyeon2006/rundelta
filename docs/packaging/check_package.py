#!/usr/bin/env python3
"""Read-only check of the actual crate, independent of Git ignore rules.

Python >=3.11. Optional --self-test exercises negative inputs in memory.
Not a proof against every possible encoding or unknown secret format.
"""
import argparse
import gzip
import hashlib
import html
import io
import json
from pathlib import Path, PurePosixPath
import re
import tarfile
import tomllib
import urllib.parse
import zlib

SOURCES = {
    'src/main.rs', 'src/cli.rs', 'src/capture/mod.rs', 'src/parse/mod.rs',
    'src/capture/completion.rs',
    'src/capture/owner.rs',
    'src/capture/reduction.rs', 'src/capture/platform_exit.rs',
    'src/capture/supervision.rs', 'src/capture/policy.rs',
    'src/parse/fs_tests.rs', 'src/model/mod.rs', 'src/store/mod.rs',
    'src/store/deletion.rs',
    'src/diff/mod.rs', 'src/report/mod.rs',
}
PAYLOAD = SOURCES | {'LICENSE', 'docs/CRATE_README.md', 'Cargo.lock', 'Cargo.toml.orig'}
EXPECTED = PAYLOAD | {'Cargo.toml'}
VCS = '.cargo_vcs_info.json'
PUBLIC_REPOSITORY = 'https://github.com/Dohyeon2006/rundelta'
LICENSE_SHA256 = 'f8e9acc75d2b3ad143bedc06b425525f311dd51b9b0cb1885dff81e1fc200a7b'
PRIVATE = re.compile(r'/(?:home|Users|root|tmp|var/tmp|var/folders|private/tmp|private/var/folders|run/user)/|[A-Za-z]:[\\/]Users[\\/]', re.I)
TOKEN = re.compile(r'gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----')
# Existing, reviewed source constants, not machine evidence. Exact quoted
# spellings in the named source file only; never a blanket temporary-root rule.
TEMP = chr(47) + 'tmp' + chr(47)
SOURCE_CONSTANTS = {
    'src/report/mod.rs': {
        TEMP+'ccAb1234.s', TEMP+'important.conf', TEMP+'ccX.s',
        TEMP+'rustcABC123/key',
    },
}


class PackageValidationError(ValueError):
    """An explicit rejection of untrusted package bytes, including under python -O."""


def require(condition, detail):
    """External validation must remain active when optimization removes assert."""
    if not condition:
        raise PackageValidationError(detail)


def decoded(text):
    text = text.replace('\\/', '/').replace('\\\\', '\\').replace('\\"', '"')
    def char(m):
        s = m[1]
        return chr(int(s[1:], 16) if s[0] in 'xuU' else int(s, 8))
    text = re.sub(r'(?<!\\)\\(x[0-9a-fA-F]{2}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|[0-7]{1,3})', char, text)
    return html.unescape(urllib.parse.unquote(text))


def privacy(name, text, private_prefixes=()):
    for _ in range(12):
        view = text
        for constant in SOURCE_CONSTANTS.get(name, ()):
            view = re.sub(r'\\*"'+re.escape(constant)+r'\\*"', '"<reviewed-source-constant>"', view)
        require(not PRIVATE.search(view), ('private path', name))
        require(not TOKEN.search(view), ('credential-shaped data', name))
        require(all(p not in view for p in private_prefixes if p), ('private prefix', name))
        for h in re.findall(r'(?<![0-9A-Za-z])(?:[0-9a-fA-F]{2}){4,}(?![0-9A-Za-z])', view):
            plain = bytes.fromhex(h).decode('utf-8', errors='replace')
            require(not PRIVATE.search(plain) and not TOKEN.search(plain), ('encoded private text', name))
            require(all(p not in plain for p in private_prefixes if p), ('encoded private prefix', name))
        next_text = decoded(text)
        if next_text == text:
            return
        text = next_text
    raise AssertionError(('excessive encoding depth', name))


def archive_stream(data):
    """Accept the observed Cargo gzip filename and exactly one complete stream.

    Raw tar remains supported for in-memory archive controls. Gzip carrier
    metadata is separate from tar member metadata and from scanned file bytes.
    """
    if data[:2] != b'\x1f\x8b':
        return io.BytesIO(data)
    require(len(data) >= 10 and data[2] == 8, 'invalid gzip header')
    flags = data[3]
    require(flags in (0, 8), 'unexpected optional gzip metadata')
    if flags == 8:
        end = data.find(b'\0', 10)
        require(end >= 0 and data[10:end] == b'RunDelta-0.2.0-alpha.1.crate',
                'unexpected gzip filename')
    inflater = zlib.decompressobj(16 + zlib.MAX_WBITS)
    position = 0
    pending = b''
    try:
        # Validate the carrier while discarding bounded output. A whole-buffer
        # expansion would defeat the member-size cap before tar could apply it.
        while True:
            if not pending and position < len(data):
                pending = data[position:position + 64 * 1024]
                position += len(pending)
            output = inflater.decompress(pending, 64 * 1024)
            pending = inflater.unconsumed_tail
            if inflater.eof:
                require(not inflater.unused_data and position == len(data),
                        'concatenated or trailing gzip stream')
                break
            if not pending and position == len(data) and not output:
                raise PackageValidationError('incomplete gzip stream')
    except zlib.error as error:
        raise PackageValidationError('invalid gzip stream') from error
    return gzip.GzipFile(fileobj=io.BytesIO(data), mode='rb')


def read_archive(data):
    files = {}
    with archive_stream(data) as stream, tarfile.open(fileobj=stream, mode='r:') as archive:
        # Cargo emits anonymous regular-file headers. Owner names and extension
        # headers are independent archive data, not covered by payload scanning.
        require(not archive.pax_headers, 'unexpected global archive metadata')
        next_header = 0
        for member in archive:
            # Even an empty PAX/GNU extension can hide header names while leaving
            # decoded metadata empty. Cargo's short names need no such headers.
            require(member.offset == next_header and member.offset_data == next_header + 512,
                    'unexpected physical archive extension')
            stream.seek(next_header)
            header = stream.read(512)
            # Both observed Cargo GNU headers and Python's USTAR controls use
            # short ASCII names with zero padding. Decoded strings drop bytes
            # after NUL, so checking only TarInfo would miss those carrier bytes.
            require(header[156:157] == tarfile.REGTYPE and
                    header[257:265] in (b'ustar  \0', b'ustar\0' + b'00'),
                    'unexpected archive header format')
            require(header[:100] == member.name.encode('ascii').ljust(100, b'\0'),
                    'noncanonical archive name padding')
            require(not any(header[157:257] + header[265:329] + header[345:512]),
                    'nonzero archive string or reserved padding')
            for start, end in ((100, 108), (108, 116), (116, 124),
                               (124, 136), (136, 148), (148, 156),
                               (329, 337), (337, 345)):
                require(re.fullmatch(rb'[0-7 ]*(?:\x00[\x00 ]*)?', header[start:end]),
                        'noncanonical archive numeric padding')
            path = PurePosixPath(member.name)
            require(bool(path.parts) and path.parts[0] == 'RunDelta-0.2.0-alpha.1' and not path.is_absolute(), 'package validation failed')
            require('..' not in path.parts and member.isfile(), 'unsafe archive member')
            require(not any((member.uname, member.gname, member.linkname,
                             member.uid, member.gid, member.devmajor, member.devminor,
                             member.pax_headers)), 'unexpected archive member metadata')
            name = '/'.join(path.parts[1:])
            require(name not in files, 'duplicate archive member')
            require(member.size < 200_000, ('unexpected large file', name))
            files[name] = archive.extractfile(member).read()
            next_header = member.offset_data + ((member.size + 511) // 512) * 512
            stream.seek(member.offset_data + member.size)
            require(not any(stream.read(next_header - stream.tell())),
                    'nonzero archive member padding')
        require(not archive.pax_headers, 'unexpected global archive metadata')
        stream.seek(next_header)
        while chunk := stream.read(64 * 1024):
            require(not any(chunk), 'nonzero trailing archive padding')
    return files


def expected_normalized_manifest(original):
    """Model only the Cargo normalization observed for this source package.

    Complete TOML comparison binds every build/dependency field to the original
    manifest. A future target, build script or normalization format needs review
    rather than silently accepting a broader transformation.
    """
    require(set(original) == {'package', 'bin', 'dependencies'},
            'unsupported original manifest sections')
    defaults = ('build', 'autolib', 'autobins', 'autoexamples', 'autotests', 'autobenches')
    require(not any(key in original['package'] for key in defaults),
            'unsupported explicit build or automatic target configuration')
    require(original['bin'] == [{'name': 'rundelta', 'path': 'src/main.rs'}],
            'unexpected original binary target')
    return {
        'package': original['package'] | dict.fromkeys(defaults, False),
        'bin': original['bin'],
        'dependencies': {
            name: {'version': value} if isinstance(value, str) else value
            for name, value in original['dependencies'].items()
        },
    }


def validate(files, source=None, private_prefixes=()):
    require(set(files) in (EXPECTED, EXPECTED | {VCS}), ('unexpected archive inventory', sorted(set(files)^EXPECTED)))
    for name, data in files.items():
        privacy(name, data.decode('utf-8'), private_prefixes)
    require(hashlib.sha256(files['LICENSE']).hexdigest() == LICENSE_SHA256, 'MIT text or copyright changed')
    for name in ('Cargo.toml', 'Cargo.toml.orig'):
        pkg = tomllib.loads(files[name].decode())['package']
        require(pkg['name'] == 'RunDelta' and pkg['version'] == '0.2.0-alpha.1', 'package validation failed')
        require(pkg['license'] == 'MIT' and pkg['readme'] == 'docs/CRATE_README.md', 'package validation failed')
        require(pkg['description'] == 'Compare observed Linux command executions and file access with strace.', 'package validation failed')
        require(pkg['keywords'] == ['cli', 'linux', 'strace', 'debugging', 'rust'], 'package validation failed')
        require(pkg['categories'] == ['command-line-utilities', 'development-tools::debugging'], 'package validation failed')
        require(pkg.get('repository') == PUBLIC_REPOSITORY, 'package repository mismatch')
        require(not any(pkg.get(k) for k in ('homepage', 'documentation')), 'package validation failed')
        require(set(pkg['include']) == {'/'+p for p in (EXPECTED - {'Cargo.toml.orig'})}, 'package validation failed')
    original = tomllib.loads(files['Cargo.toml.orig'].decode())
    normalized = tomllib.loads(files['Cargo.toml'].decode())
    # JSON retains TOML scalar type distinctions such as false versus zero;
    # ordinary Python mapping equality would accept that invalid substitution.
    require(json.dumps(normalized, sort_keys=True) ==
            json.dumps(expected_normalized_manifest(original), sort_keys=True),
            'normalized manifest changes source build or dependency semantics')
    if VCS in files:
        vcs = json.loads(files[VCS])
        require(vcs['path_in_vcs'] == '' and not vcs['git'].get('dirty', False), 'package validation failed')
        require(re.fullmatch('[0-9a-f]{40}', vcs['git']['sha1']), 'package validation failed')
    if source is not None:
        for name in PAYLOAD:
            original = 'Cargo.toml' if name == 'Cargo.toml.orig' else name
            require(files[name] == (source/original).read_bytes(), ('source mismatch', name))


def self_test(valid):
    count = 0
    def reject(fn):
        nonlocal count
        try:
            fn()
        except (AssertionError, ValueError):
            count += 1
        else:
            raise AssertionError('negative example was accepted')
    for name in ('.idea/private.xml', 'target/program', 'docs/external-alpha/evidence.json',
                 'tests/fixtures/trace.txt', 'README.md', 'docs/ALPHA_READINESS.md'):
        reject(lambda name=name: validate(valid | {name: b'synthetic'}))
    private = chr(47)+'home'+chr(47)+'example'+chr(47)+'secret'
    escaped = ''.join('\\x%02x'%b for b in private.encode())
    for text in (private, escaped, json.dumps(json.dumps(escaped)), private.encode().hex(),
                 ''.join('\\u%04x'%ord(c) for c in private), urllib.parse.quote(private, safe='')):
        reject(lambda text=text: privacy('docs/CRATE_README.md', text))
    reject(lambda: privacy('docs/CRATE_README.md', 'gh'+'p_'+'a'*30))
    reject(lambda: privacy('src/report/mod.rs', '"'+TEMP+'ccX.s/extra"'))
    reject(lambda: privacy('docs/CRATE_README.md', '"'+TEMP+'ccX.s"'))
    reject(lambda: validate(valid | {'LICENSE': b'wrong license'}))
    reject(lambda: validate(valid | {'Cargo.toml': valid['Cargo.toml'].replace(b'license = "MIT"', b'license = "unknown"')}))
    def manifest_bytes(manifest):
        # The synthetic controls use the supported package/bin/dependency data,
        # with deliberately different ordering from Cargo's textual formatting.
        lines = []
        def table(header, fields):
            lines.append(header)
            lines.extend(json.dumps(key) + ' = ' + json.dumps(value)
                         for key, value in sorted(fields.items()))
        table('[package]', manifest['package'])
        for target in manifest['bin']:
            table('[[bin]]', target)
        for name, fields in sorted(manifest['dependencies'].items()):
            table('[dependencies.' + json.dumps(name) + ']', fields)
        return ('\n'.join(lines) + '\n').encode()
    normalized = tomllib.loads(valid['Cargo.toml'].decode())
    validate(valid | {'Cargo.toml': manifest_bytes(normalized)})
    for section, field, value in (
        ('bin', 'name', 'changed_binary'), ('bin', 'path', 'src/cli.rs'),
        ('package', 'build', 'build.rs'), ('package', 'autobins', True),
        ('package', 'build', 0), ('libc', 'version', '0.3'),
        ('clap', 'features', ['different_feature']),
    ):
        changed = json.loads(json.dumps(normalized))
        target = (changed['bin'][0] if section == 'bin' else changed['package']
                  if section == 'package' else changed['dependencies'][section])
        target[field] = value
        reject(lambda changed=changed: validate(valid | {'Cargo.toml': manifest_bytes(changed)}))
    for extra in (
        b'\n[build-dependencies.unexpected]\nversion = "1"\n',
        b'\n[profile.release]\nstrip = true\n',
    ):
        reject(lambda extra=extra: validate(valid | {'Cargo.toml': valid['Cargo.toml'] + extra}))
    vcs = dict(git=dict(sha1='0'*40, dirty=True), path_in_vcs='')
    reject(lambda: validate(valid | {VCS: json.dumps(vcs).encode()}))
    # Exercise the archive reader independently of the decoded inventory. These
    # controls remain active under -O just like the actual external validation.
    def archive(entries, *, metadata=None, global_headers=None):
        payload = io.BytesIO()
        with tarfile.open(fileobj=payload, mode='w', format=tarfile.PAX_FORMAT,
                          pax_headers=global_headers) as output:
            for name, kind, size in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = size
                for field, value in (metadata or {}).items():
                    setattr(member, field, value)
                output.addfile(member, io.BytesIO(b'x' * size) if kind == tarfile.REGTYPE else None)
        data = bytearray(payload.getvalue())
        # tarfile's writer omits device numbers on regular files. Construct the
        # otherwise valid USTAR header so those external-byte checks are tested.
        altered = False
        for field, offset in (('devmajor', 329), ('devminor', 337)):
            if (metadata or {}).get(field):
                data[offset:offset+8] = ('%07o\0' % metadata[field]).encode()
                altered = True
        if altered:
            data[148:156] = b'        '
            data[148:156] = ('%06o\0 ' % sum(data[:512])).encode()
        return bytes(data)
    prefix = 'RunDelta-0.2.0-alpha.1/'
    for name in ('/absolute', prefix+'../outside', 'wrong-root/file'):
        reject(lambda name=name: read_archive(archive([(name, tarfile.REGTYPE, 1)])))
    reject(lambda: read_archive(archive([(prefix+'linked', tarfile.SYMTYPE, 0)])))
    reject(lambda: read_archive(archive([(prefix+'large', tarfile.REGTYPE, 200_000)])))
    reject(lambda: read_archive(archive([(prefix+'same', tarfile.REGTYPE, 1)] * 2)))
    ordinary = [(prefix+'ordinary', tarfile.REGTYPE, 1)]
    for field, value in (
        ('uname', 'synthetic-owner'), ('gname', 'synthetic-group'),
        ('linkname', 'synthetic-link'), ('uid', 1), ('gid', 1),
        ('devmajor', 1), ('devminor', 1),
        ('pax_headers', {'comment': private}),
    ):
        reject(lambda field=field, value=value:
               read_archive(archive(ordinary, metadata={field: value})))
    reject(lambda: read_archive(archive(ordinary, global_headers={'comment': private})))
    plain = archive(ordinary)
    require(read_archive(plain) == {'ordinary': b'x'},
            'ordinary Cargo-shaped member metadata was rejected')
    for offset in (len((prefix+'ordinary').encode()) + 1, 158, 266, 298,
                   346, 501, 138):
        hidden = bytearray(plain)
        # mtime is a numeric field: an early NUL would otherwise discard its
        # suffix just like a string field, while retaining a valid parsed value.
        if offset == 138:
            hidden[136:148] = b'0\0/home/x/\0\0'
        else:
            hidden[offset:offset+8] = b'/home/x/'
        hidden[148:156] = b'        '
        hidden[148:156] = ('%06o\0 ' % sum(hidden[:512])).encode()
        reject(lambda hidden=hidden: read_archive(bytes(hidden)))
    for kind in (tarfile.XHDTYPE, tarfile.XGLTYPE):
        hidden = tarfile.TarInfo(private)
        hidden.type = kind
        reject(lambda hidden=hidden: read_archive(hidden.tobuf(format=tarfile.USTAR_FORMAT) + plain))
    padding = bytearray(plain)
    padding[513:513+len(private)] = private.encode()
    reject(lambda: read_archive(bytes(padding)))
    reject(lambda: read_archive(plain + private.encode()))
    for filename in ('', 'RunDelta-0.2.0-alpha.1.crate'):
        wrapped = io.BytesIO()
        with gzip.GzipFile(filename=filename, fileobj=wrapped, mode='wb', mtime=0) as output:
            output.write(plain)
        require(read_archive(wrapped.getvalue()) == {'ordinary': b'x'},
                'ordinary Cargo-shaped gzip metadata was rejected')
    compressed = gzip.compress(plain, mtime=0)
    for flags, fields in (
        (8, ('gh'+'p_'+'a'*30).encode() + b'\0'),
        (16, private.encode() + b'\0'),
        (4, b'\x01\x00x'), (2, b'\x00\x00'), (32, b''),
    ):
        header = compressed[:3] + bytes([flags]) + compressed[4:10]
        if flags == 2:
            fields = (zlib.crc32(header) & 0xffff).to_bytes(2, 'little')
        altered = header + fields + compressed[10:]
        reject(lambda altered=altered: read_archive(altered))
    reject(lambda: read_archive(compressed + gzip.compress(private.encode())))
    reject(lambda: read_archive(compressed + private.encode()))
    reject(lambda: read_archive(compressed[:-1]))
    validate(valid)
    for name, values in SOURCE_CONSTANTS.items():
        for value in values:
            privacy(name, '"'+value+'"')
    print('PASS:', count, 'negative counterexamples; actual clean inventory and exact source constants accepted')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path)
    parser.add_argument('--source', type=Path, help='Compare original payload bytes with this clean source tree')
    parser.add_argument('--private-prefix', action='append', default=[])
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    files = read_archive(args.archive.read_bytes())
    validate(files, args.source, args.private_prefix)
    if args.self_test:
        self_test(files)
    print('PASS:', len(files), 'archive files; exact allowlist, MIT metadata/text, source bytes and decoded privacy checks')
    print('\n'.join(sorted(files)))
