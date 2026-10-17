# Retry registered before execution
Initial Python 3.10.12 failed before SPAdes started: missing libcrypt.so.1, both target return codes 127. These are bootstrap failures, not a normal/failure pair and not an original-issue reproduction.
Retry with publicly downloadable CPython 3.10.19. Keep SPAdes 4.0.0 and identical installation bytes, space/no-space contrast, preregistered clue and success criteria. Report the Python patch-level deviation. New run names good-v2/bad-v2; preserve original records.
