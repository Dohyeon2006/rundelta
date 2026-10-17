#define _GNU_SOURCE

#include <errno.h>
#include <stdarg.h>

/* Force the shared child pre_exec boundary to report an exec-handshake error. */
int prctl(int option, ...) {
    (void)option;
    errno = EPERM;
    return -1;
}
