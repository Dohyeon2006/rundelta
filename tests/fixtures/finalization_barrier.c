#define _GNU_SOURCE

/* Test-only publication barrier. No production hooks or real target are used.
 * The owned recorder pauses after events publication, after capture facts were
 * frozen. A FIFO releases it; optional marker failure covers error restoration. */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int renameat(int old_dir, const char *old_name, int new_dir, const char *new_name) {
    int (*real_renameat)(int, const char *, int, const char *) =
        dlsym(RTLD_NEXT, "renameat");
    if (!real_renameat) _exit(97);
    const char *ready = getenv("RF_FINALIZE_READY");
    const char *release = getenv("RF_FINALIZE_RELEASE");
    if (ready && release && getenv("RF_FINALIZE_FAIL") &&
        strcmp(new_name, "finalized.json") == 0) {
        errno = EIO;
        return -1;
    }
    int result = real_renameat(old_dir, old_name, new_dir, new_name);
    int saved_errno = errno;
    if (result == 0 && ready && release && strcmp(new_name, "events.jsonl") == 0) {
        int marker = open(ready, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
        if (marker < 0 || close(marker) < 0) _exit(98);
        int gate = open(release, O_RDONLY | O_CLOEXEC);
        if (gate < 0) _exit(98);
        char byte;
        ssize_t count;
        do { count = read(gate, &byte, 1); } while (count < 0 && errno == EINTR);
        if (count != 1 || close(gate) < 0) _exit(98);
    }
    errno = saved_errno;
    return result;
}
