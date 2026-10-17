#!/usr/bin/env python3
"""Synthetic privacy-counterexamples, generated in memory; no real user paths."""
import json
import unittest
import urllib.parse
from verify import check_text


class Redaction(unittest.TestCase):
    def private(self):
        return chr(47) + 'home' + chr(47) + 'example' + chr(47) + 'secret.txt'

    def reject(self, value):
        with self.assertRaises(AssertionError):
            check_text(value)

    def test_plaintext(self):
        self.reject(self.private())
        self.reject('evidence: "' + self.private() + '"')
        self.reject('file://' + self.private())

    def test_strace_hex_and_bare_hex(self):
        self.reject(''.join('\\x%02x' % b for b in self.private().encode()))
        self.reject(self.private().encode().hex())

    def test_unicode(self):
        self.reject(''.join('\\u%04x' % ord(c) for c in self.private()))
        self.reject(''.join('\\U%08x' % ord(c) for c in self.private()))

    def test_octal(self):
        self.reject(''.join('\\%03o' % ord(c) for c in self.private()))

    def test_json_nested_escaping(self):
        value = ''.join('\\x%02x' % b for b in self.private().encode())
        for _ in range(3):
            value = json.dumps(value)
            self.reject(value)

    def test_url_and_escaped_slashes(self):
        self.reject(urllib.parse.quote(self.private(), safe=''))
        self.reject(self.private().replace('/', '\\/'))

    def test_other_private_roots_and_credentials(self):
        for root in ('tmp', 'var/tmp', 'run/user', 'Users', 'root', 'private/tmp', 'var/folders'):
            self.reject(chr(47) + root + chr(47) + 'synthetic')
        self.reject('C:' + chr(92) + 'Users' + chr(92) + 'synthetic')
        self.reject('gh' + 'p_' + 'A' * 30)

    def test_safe_placeholders_and_system_paths(self):
        for value in ('${CASE}/home/.config/pip/pip.conf', '${WORK}/tools/cmake/data/bin/cmake',
                      '/usr/bin/as', 'raw/trace.log:11', 'relative:cmake [cwd unresolved]',
                      'https://github.com/pypa/pip/issues/12996'):
            check_text(value)


if __name__ == '__main__':
    unittest.main(verbosity=2)
