#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <fcntl.h>

static atomic_int ready, go, entered, stop;
static int file_mode;
static void *worker(void *unused) {
    (void)unused;
    atomic_fetch_add(&ready, 1);
    while (!atomic_load(&go)) { }
    atomic_fetch_add(&entered, 1);
    while (!atomic_load(&stop)) {
        if (file_mode) {
            int fd = open("/dev/null", O_RDONLY);
            if (fd >= 0) close(fd);
        } else {
            syscall(SYS_getpid);
        }
    }
    return NULL;
}
int main(int argc, char **argv) {
    if (argc != 4) return 64;
    int joined = atoi(argv[1]);
    int count = atoi(argv[2]);
    file_mode = atoi(argv[3]);
    if (count < 1 || count > 64) return 64;
    pthread_t threads[64];
    for (int i = 0; i < count; i++)
        if (pthread_create(&threads[i], NULL, worker, NULL)) return 70;
    while (atomic_load(&ready) != count) { }
    atomic_store(&go, 1);
    while (atomic_load(&entered) != count) { }
    for (int i = 0; i < 32; i++) syscall(SYS_getpid);
    if (joined) {
        atomic_store(&stop, 1);
        for (int i = 0; i < count; i++)
            if (pthread_join(threads[i], NULL)) return 71;
    }
    syscall(SYS_exit_group, 0);
    return 72;
}
