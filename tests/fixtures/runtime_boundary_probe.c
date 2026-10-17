#define _POSIX_C_SOURCE 200809L

/* Characterize process state at the target's exec boundary. This is a test
 * fixture, not a launcher or compatibility promise shipped by RunDelta. */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int set_disposition(int signal_number, void (*handler)(int)) {
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = handler;
    if (sigemptyset(&action.sa_mask) != 0) {
        return -1;
    }
    return sigaction(signal_number, &action, NULL);
}

static int configure_signals(const char *mode) {
    const int observed[] = {SIGHUP, SIGINT, SIGTERM, SIGPIPE};
    sigset_t empty;
    size_t index;

    if (sigemptyset(&empty) != 0 || sigprocmask(SIG_SETMASK, &empty, NULL) != 0) {
        return -1;
    }
    for (index = 0; index < sizeof(observed) / sizeof(observed[0]); ++index) {
        if (set_disposition(observed[index], SIG_DFL) != 0) {
            return -1;
        }
    }
    if (strcmp(mode, "neutral") == 0 || strcmp(mode, "pipe-default") == 0) {
        return 0;
    }
    if (strcmp(mode, "ignore-hup") == 0) {
        return set_disposition(SIGHUP, SIG_IGN);
    }
    if (strcmp(mode, "ignore-int") == 0) {
        return set_disposition(SIGINT, SIG_IGN);
    }
    if (strcmp(mode, "ignore-term") == 0) {
        return set_disposition(SIGTERM, SIG_IGN);
    }
    if (strcmp(mode, "block-hup") == 0 || strcmp(mode, "block-int") == 0 ||
        strcmp(mode, "block-term") == 0) {
        int number = strcmp(mode, "block-hup") == 0 ? SIGHUP :
            (strcmp(mode, "block-int") == 0 ? SIGINT : SIGTERM);
        sigset_t blocked;
        if (sigemptyset(&blocked) != 0 || sigaddset(&blocked, number) != 0) {
            return -1;
        }
        return sigprocmask(SIG_BLOCK, &blocked, NULL);
    }
    if (strcmp(mode, "pipe-ignore") == 0) {
        return set_disposition(SIGPIPE, SIG_IGN);
    }
    errno = EINVAL;
    return -1;
}

static char disposition(int signal_number) {
    struct sigaction action;
    if (sigaction(signal_number, NULL, &action) != 0) {
        return 'E';
    }
    if (action.sa_handler == SIG_DFL) {
        return 'D';
    }
    if (action.sa_handler == SIG_IGN) {
        return 'I';
    }
    return 'H';
}

static int descriptor_is_open(int descriptor) {
    int result;
    do {
        errno = 0;
        result = fcntl(descriptor, F_GETFD);
    } while (result < 0 && errno == EINTR);
    return result >= 0 || errno != EBADF;
}

static int signal_is_blocked(int signal_number) {
    sigset_t mask;
    int member;
    if (sigprocmask(SIG_SETMASK, NULL, &mask) != 0) {
        return -1;
    }
    member = sigismember(&mask, signal_number);
    return member;
}

static int write_all(int descriptor, const char *bytes, size_t length) {
    while (length > 0) {
        ssize_t written = write(descriptor, bytes, length);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return -1;
        }
        bytes += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

static int target(const char *report_path) {
    int fd0 = descriptor_is_open(STDIN_FILENO);
    int fd1 = descriptor_is_open(STDOUT_FILENO);
    int fd2 = descriptor_is_open(STDERR_FILENO);
    int blocked_hup = signal_is_blocked(SIGHUP);
    int blocked_int = signal_is_blocked(SIGINT);
    int blocked_term = signal_is_blocked(SIGTERM);
    char report[256];
    int length = snprintf(
        report,
        sizeof(report),
        "hup=%c int=%c term=%c pipe=%c blocked_hup=%d blocked_int=%d blocked_term=%d fd0=%d fd1=%d fd2=%d\n",
        disposition(SIGHUP),
        disposition(SIGINT),
        disposition(SIGTERM),
        disposition(SIGPIPE),
        blocked_hup,
        blocked_int,
        blocked_term,
        fd0,
        fd1,
        fd2
    );
    int output;
    int result;

    if (length < 0 || (size_t)length >= sizeof(report)) {
        return 90;
    }
    output = open(report_path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (output < 0) {
        return 91;
    }
    result = write_all(output, report, (size_t)length);
    if (close(output) != 0 && result == 0) {
        result = -1;
    }
    return result == 0 ? 0 : 92;
}

static int target_wait(const char *report_path, const char *ready_path) {
    char ready[64];
    int output;
    int length;
    int result = target(report_path);
    if (result != 0) {
        return result;
    }
    output = open(ready_path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
    if (output < 0) {
        return 97;
    }
    length = snprintf(ready, sizeof(ready), "%ld\n", (long)getpid());
    if (length < 0 || (size_t)length >= sizeof(ready) ||
        write_all(output, ready, (size_t)length) != 0) {
        close(output);
        return 98;
    }
    if (close(output) != 0) {
        return 99;
    }
    for (;;) {
        pause();
    }
}

static int launcher(int argc, char **argv) {
    char *end = NULL;
    long close_mask;
    int descriptor;

    if (argc < 5 || configure_signals(argv[2]) != 0) {
        return 93;
    }
    errno = 0;
    close_mask = strtol(argv[3], &end, 0);
    if (errno != 0 || end == argv[3] || *end != '\0' || close_mask < 0 || close_mask > 7) {
        return 94;
    }
    for (descriptor = 0; descriptor <= 2; ++descriptor) {
        if ((close_mask & (1L << descriptor)) != 0) {
            while (close(descriptor) != 0 && errno == EINTR) {
            }
        }
    }
    execvp(argv[4], &argv[4]);
    return 95;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "target") == 0) {
        return target(argv[2]);
    }
    if (argc == 4 && strcmp(argv[1], "target-wait") == 0) {
        return target_wait(argv[2], argv[3]);
    }
    if (argc >= 5 && strcmp(argv[1], "launch") == 0) {
        return launcher(argc, argv);
    }
    return 96;
}
