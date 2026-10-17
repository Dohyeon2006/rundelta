#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

/* The coordinator uses pipes, so no test relies on a scheduling delay. */
static int ready_pipe[2], done_pipe[2];
static const char *mode, *operation, *destination, *program;
static char expected;

static int transfer(int fd, int send) {
    char value = 'x';
    ssize_t n;
    do { n = send ? write(fd, &value, 1) : read(fd, &value, 1); } while (n < 0 && errno == EINTR);
    return n == 1 ? 0 : 70;
}

static int observe(void) {
    int fd = openat(AT_FDCWD, "payload", O_RDONLY);
    char value = '?';
    if (fd < 0 || read(fd, &value, 1) != 1) return 71;
    if (close(fd)) return 72;
    printf("OBSERVE tid=%ld marker=%c expected=%c\n", syscall(SYS_gettid), value, expected);
    fflush(stdout);
    return value == expected ? 0 : 73;
}

static int reader(void *unused) {
    (void)unused;
    if (!strcmp(mode, "unshare") && unshare(CLONE_FS)) return 74;
    if (!strcmp(mode, "exec-shared") || !strcmp(mode, "exec-private")) {
        char ready[32], done[32], marker[2] = {expected, 0};
        snprintf(ready, sizeof ready, "%d", ready_pipe[1]);
        snprintf(done, sizeof done, "%d", done_pipe[0]);
        execl(program, program, "exec-reader", ready, done, marker, NULL);
        return 75;
    }
    int rc = transfer(ready_pipe[1], 1);
    return rc ? rc : (transfer(done_pipe[0], 0) ? 76 : observe());
}

static void *thread_reader(void *unused) {
    return (void *)(intptr_t)reader(unused);
}

static void *noop(void *unused) { return unused; }

static int change_directory(void) {
    int fd = -1;
    int rc;
    if (!strcmp(operation, "none")) return 0;
    if (!strcmp(operation, "chdir") || !strcmp(operation, "fail-chdir")) rc = chdir(destination);
    else if (!strcmp(operation, "fchdir")) {
        fd = open(destination, O_RDONLY | O_DIRECTORY);
        if (fd < 0) return 77;
        rc = fchdir(fd);
    } else if (!strcmp(operation, "fail-fchdir")) rc = fchdir(-1);
    else return 78;
    int saved = rc ? errno : 0;
    if (fd >= 0) close(fd);
    printf("CHANGE tid=%ld operation=%s rc=%d errno=%d\n",
           syscall(SYS_gettid), operation, rc, saved);
    fflush(stdout);
    if (!strncmp(operation, "fail-", 5)) return rc == -1 ? 0 : 79;
    return rc ? 80 : 0;
}

int main(int argc, char **argv) {
    if (argc == 5 && !strcmp(argv[1], "exec-reader")) {
        expected = argv[4][0];
        if (transfer(atoi(argv[2]), 1) || transfer(atoi(argv[3]), 0)) return 81;
        return observe();
    }
    if (argc != 5) return 64;
    mode = argv[1]; operation = argv[2]; destination = argv[3]; expected = argv[4][0]; program = argv[0];
    if (!strcmp(mode, "normal")) return !strcmp(operation, "none") ? observe() : change_directory();
    if (pipe(ready_pipe) || pipe(done_pipe)) return 82;
    pthread_t thread;
    pid_t child = -1;
    void *stack = NULL;
    if (!strcmp(mode, "pthread")) {
        if (pthread_create(&thread, NULL, thread_reader, NULL)) return 83;
    } else if (!strcmp(mode, "fork") || !strcmp(mode, "mixed") || !strcmp(mode, "exec-private")) {
        child = fork();
        if (child == 0) _exit(reader(NULL));
        if (child < 0) return 84;
    } else if (!strcmp(mode, "clonefs") || !strcmp(mode, "unshare") || !strcmp(mode, "exec-shared")) {
        stack = malloc(1024 * 1024);
        if (!stack) return 85;
        child = clone(reader, (char *)stack + 1024 * 1024, CLONE_FS | SIGCHLD, NULL);
        if (child < 0) return 86;
    } else return 64;
    if (transfer(ready_pipe[0], 0)) return 87;
    if (!strcmp(mode, "mixed")) {
        pthread_t unrelated;
        if (pthread_create(&unrelated, NULL, noop, NULL) || pthread_join(unrelated, NULL)) return 88;
    }
    int rc = change_directory();
    if (transfer(done_pipe[1], 1)) return 89;
    int reader_rc;
    if (!strcmp(mode, "pthread")) {
        void *result;
        if (pthread_join(thread, &result)) return 90;
        reader_rc = (int)(intptr_t)result;
    } else {
        int status;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 91;
        reader_rc = WEXITSTATUS(status);
    }
    free(stack);
    return rc ? rc : reader_rc;
}
