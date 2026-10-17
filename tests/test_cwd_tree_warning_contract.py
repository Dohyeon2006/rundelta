"""Positive and fail-closed oracles for the independent cwd-tree contract."""
import unittest
from cwd_tree_warning_contract import audit_directory_warnings


class DirectoryWarnings(unittest.TestCase):
    def fixture(self):
        raw = ['20 chdir("parent") = 0', '21 chdir("../child") = 0',
               '20 chdir("does-not-exist") = -1 ENOENT (No such file or directory)']
        warnings = ['raw/trace.log:1: fs.context: explanation',
                    'raw/trace.log:2: fs.context: changed explanation',
                    'raw/trace.log:3: path context unresolved; explanation']
        return warnings, raw

    def test_explanatory_suffix_is_not_the_contract(self):
        warnings, raw = self.fixture()
        audit_directory_warnings(warnings, raw)
        audit_directory_warnings([warning + '; reworded tail' for warning in warnings], raw)

    def test_missing_extra_wrong_categories_and_references_fail_closed(self):
        warnings, raw = self.fixture()
        mutations = [warnings[:-1], warnings + [warnings[0]],
                     [warnings[0].replace('fs.context:', 'unknown:'), *warnings[1:]],
                     [warnings[0].replace('fs.context:', 'path context unresolved;'), *warnings[1:]]]
        for reference in ['other.log:1', 'raw/trace.log:0', 'raw/trace.log:99', 'raw/trace.log:2']:
            mutations.append([warnings[0].replace('raw/trace.log:1', reference), *warnings[1:]])
        for mutation in mutations:
            with self.subTest(warnings=mutation), self.assertRaises(AssertionError):
                audit_directory_warnings(mutation, raw)
        for index, replacement in [(0, '20 chdir("parent") = -1 ENOENT'),
                                   (1, '21 chdir("other") = 0'),
                                   (2, '20 chdir("does-not-exist") = 0')]:
            lines = raw.copy()
            lines[index] = replacement
            with self.subTest(raw=lines), self.assertRaises(AssertionError):
                audit_directory_warnings(warnings, lines)

if __name__ == '__main__':
    unittest.main(verbosity=2)
