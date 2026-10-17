#!/usr/bin/env python3
"""Synthetic trace producer, NOT strace. Never launches the supplied target.
Used only to feed controlled inputs into RunDelta's unmodified record path.
"""
import os
import shutil
import sys
if sys.argv[1:]==['--version']:
    print('A3 synthetic fixture producer (not strace; no real target execution)')
else:
    destination=sys.argv[sys.argv.index('-o')+1]
    shutil.copyfile(os.environ['A3_TRACE_INPUT'],destination)
