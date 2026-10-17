#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int ready_pipe[2], done_pipe[2], extra_access;
static char expected;
static int transfer(int fd, int send) {
    char value = 'x';
    ssize_t n;
    do { n = send ? write(fd, &value, 1) : read(fd, &value, 1); } while (n < 0 && errno == EINTR);
    return n == 1 ? 0 : 70;
}
static int observe(void) {
    int fd = openat(AT_FDCWD, "payload.txt", O_RDONLY);
    char value = '?';
    if (fd < 0 || read(fd, &value, 1) != 1) return 71;
    if (close(fd)) return 72;
    int access_rc = 99, missing_rc = 99, missing_errno = 0;
    if (extra_access) {
        access_rc = access("payload.txt", R_OK);
        missing_rc = access("not-present.txt", F_OK);
        missing_errno = errno;
    }
    dprintf(STDOUT_FILENO, "reader_tid=%ld marker=%c expected=%c access=%d missing=%d missing_errno=%d\n",
            syscall(SYS_gettid), value, expected, access_rc, missing_rc, missing_errno);
    if (value != expected) return 73;
    if (extra_access && (access_rc != 0 || missing_rc != -1 || missing_errno != ENOENT)) return 74;
    return 0;
}
static int reader(void *unused) {
    (void)unused;
    int rc = transfer(ready_pipe[1], 1);
    if (rc) return rc;
    rc = transfer(done_pipe[0], 0);
    return rc ? rc : observe();
}
static void *thread_reader(void *arg) { return (void *)(intptr_t)reader(arg); }
static void *no_cwd_change(void *arg) { return arg; }
int main(int argc, char **argv) {
    if (argc != 6) return 64;
    const char *mode = argv[1], *operation = argv[2], *destination = argv[3];
    expected = argv[4][0]; extra_access = atoi(argv[5]);
    if (!strcmp(mode, "normal")) return observe();
    int dirfd = -1;
    if (!strcmp(operation, "fchdir")) {
        dirfd = open(destination, O_RDONLY | O_DIRECTORY);
        if (dirfd < 0) return 75;
    }
    if (pipe(ready_pipe) || pipe(done_pipe)) return 76;
    pthread_t thread;
    pid_t child = -1;
    void *stack = NULL;
    if (!strcmp(mode, "pthread")) {
        if (pthread_create(&thread, NULL, thread_reader, NULL)) return 77;
    } else if (!strcmp(mode, "fork") || !strcmp(mode, "mixed")) {
        child = fork();
        if (child < 0) return 78;
        if (child == 0) _exit(reader(NULL));
    } else if (!strcmp(mode, "clonefs")) {
        stack = malloc(1024 * 1024);
        if (!stack) return 79;
        child = clone(reader, (char *)stack + 1024 * 1024, CLONE_FS | SIGCHLD, NULL);
        if (child < 0) return 80;
    } else return 64;
    if (transfer(ready_pipe[0], 0)) return 81;
    if (!strcmp(mode, "mixed")) {
        pthread_t other;
        if (pthread_create(&other, NULL, no_cwd_change, NULL) || pthread_join(other, NULL)) return 82;
    }
    int rc = 0, saved_errno = 0;
    if (!strcmp(operation, "chdir") || !strcmp(operation, "fail-chdir")) rc = chdir(destination);
    else if (!strcmp(operation, "fchdir")) rc = fchdir(dirfd);
    else if (!strcmp(operation, "fail-fchdir")) rc = fchdir(-1);
    else if (strcmp(operation, "none")) return 64;
    saved_errno = rc < 0 ? errno : 0;
    dprintf(STDOUT_FILENO, "writer_tid=%ld operation=%s result=%d errno=%d\n",
            syscall(SYS_gettid), operation, rc, saved_errno);
    int correct = !strncmp(operation, "fail-", 5)
        ? rc == -1 && saved_errno == (!strcmp(operation, "fail-chdir") ? ENOENT : EBADF)
        : rc == 0;
    if (dirfd >= 0) close(dirfd);
    if (transfer(done_pipe[1], 1)) return 83;
    int reader_rc = 84;
    if (!strcmp(mode, "pthread")) {
        void *result;
        if (pthread_join(thread, &result)) return 85;
        reader_rc = (int)(intptr_t)result;
    } else {
        int status;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 86;
        reader_rc = WEXITSTATUS(status);
        free(stack);
    }
    for (int i = 0; i < 2; i++) { close(ready_pipe[i]); close(done_pipe[i]); }
    return !correct ? 87 : reader_rc;
}
