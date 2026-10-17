#!/usr/bin/env python3
"""Explicit synthetic trace backend for FsContext CLI contract tests only."""
import os
import shutil
import sys

if "--version" in sys.argv[1:]:
    print("FsContext synthetic trace backend; does not execute the target")
else:
    shutil.copyfile(os.environ["FS_TRACE"], sys.argv[sys.argv.index("-o") + 1])
