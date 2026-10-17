#define _GNU_SOURCE

/* Private LD_PRELOAD regression probe, never a RunDelta production hook.
 * Only the named recorder image may arm a failure, after successful
 * finalization-marker publication. A forked child inherits globals but lacks
 * the owning PID; an exec starts its own unarmed image. One-shot modes permit
 * Drop recovery; permanent modes refuse every restoration and audit that fact.
 */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int (*next_renameat)(int, const char *, int, const char *);
static int (*next_sigaction)(int, const struct sigaction *, struct sigaction *);
static int (*next_sigprocmask)(int, const sigset_t *, sigset_t *);
static int (*next_pthread_sigmask)(int, const sigset_t *, sigset_t *);
static const int watched[] = {SIGHUP, SIGINT, SIGTERM};
static struct sigaction original_actions[3];
static sigset_t original_mask;
static pid_t owner;
static int selected_mode, published, injected;
static const char *audit_path;
static const char *snapshot_path;

/* Each audit line uses one append write, preserving subprocess boundaries.
 * The finite fixture treats short writes as errors rather than hiding evidence.
 */
static void audit(const char *event, const char *role) {
    if (audit_path == NULL) return;
    char line[160];
    int length;
    if (strncmp(event, "injected-", 9) == 0) {
        int pthread_error = strcmp(event, "injected-pthread_sigmask") == 0;
        length = snprintf(line, sizeof(line),
                          "event=%s pid=%ld role=%s result=%d errno=%d count=%d\n",
                          event, (long)getpid(), role,
                          pthread_error ? EIO : -1, pthread_error ? errno : EIO, injected);
    } else {
        length = snprintf(line, sizeof(line), "event=%s pid=%ld role=%s\n",
                          event, (long)getpid(), role);
    }
    if (length < 0 || (size_t)length >= sizeof(line)) _exit(96);
    int descriptor = open(audit_path, O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC, 0600);
    if (descriptor < 0) _exit(96);
    ssize_t written;
    do { written = write(descriptor, line, (size_t)length); }
    while (written < 0 && errno == EINTR);
    if (written != length || close(descriptor) != 0) _exit(96);
}

static int is_owner(void) {
    return owner != 0 && getpid() == owner;
}

static void action_mismatch(int number, const struct sigaction *actual,
                            const struct sigaction *expected) {
    char event[100];
    int length = snprintf(event, sizeof(event),
                          "action-mismatch-%d-flags-%x-expected-%x-handler-%d",
                          number, (unsigned)actual->sa_flags, (unsigned)expected->sa_flags,
                          actual->sa_handler == expected->sa_handler);
    if (length < 0 || (size_t)length >= sizeof(event)) _exit(96);
    audit(event, "recorder");
}

static void initialized_action(int number, const struct sigaction *raw,
                               const struct sigaction *initialized) {
    int same_mask = 1;
    for (int member = 1; member < NSIG; ++member) {
        same_mask &= sigismember(&raw->sa_mask, member) ==
                     sigismember(&initialized->sa_mask, member);
    }
    char event[110];
    int length = snprintf(event, sizeof(event),
                          "fixture-action-%d-raw-flags-%x-initial-flags-%x-handler-%d-mask-%d",
                          number, (unsigned)raw->sa_flags, (unsigned)initialized->sa_flags,
                          raw->sa_handler == initialized->sa_handler, same_mask);
    if (length < 0 || (size_t)length >= sizeof(event)) _exit(96);
    audit(event, "fixture");
}

static void resolve_functions(void) {
    if (next_renameat == NULL) next_renameat = dlsym(RTLD_NEXT, "renameat");
    if (next_sigaction == NULL) next_sigaction = dlsym(RTLD_NEXT, "sigaction");
    if (next_sigprocmask == NULL) next_sigprocmask = dlsym(RTLD_NEXT, "sigprocmask");
    if (next_pthread_sigmask == NULL)
        next_pthread_sigmask = dlsym(RTLD_NEXT, "pthread_sigmask");
    if (next_renameat == NULL || next_sigaction == NULL ||
        next_sigprocmask == NULL || next_pthread_sigmask == NULL) _exit(96);
}

__attribute__((constructor)) static void prepare(void) {
    resolve_functions();
    audit_path = getenv("RD_RESTORE_AUDIT");
    snapshot_path = getenv("RD_RESTORE_SNAPSHOT");
    const char *recorder = getenv("RD_RESTORE_RECORDER");
    const char *mode = getenv("RD_RESTORE_MODE");
    char image[PATH_MAX];
    ssize_t length = readlink("/proc/self/exe", image, sizeof(image) - 1);
    if (length < 0 || recorder == NULL || mode == NULL || snapshot_path == NULL) return;
    image[length] = '\0';
    if (strcmp(image, recorder) != 0) {
        audit("process", "child");
        return;
    }
    if (strcmp(mode, "none") == 0) selected_mode = 0;
    else if (strcmp(mode, "action") == 0) selected_mode = 1;
    else if (strcmp(mode, "mask") == 0) selected_mode = 2;
    else if (strcmp(mode, "permanent-action") == 0) selected_mode = 3;
    else if (strcmp(mode, "permanent-mask") == 0) selected_mode = 4;
    else _exit(96);
    owner = getpid();
    if (next_sigprocmask(SIG_SETMASK, NULL, &original_mask) != 0) _exit(96);
    audit("process", "recorder");
    for (size_t index = 0; index < 3; ++index) {
        struct sigaction raw;
        // Establish this fixture's actions through the same real libc API used
        // for restoration, then query its normalized defined state. Linux exec
        // can leave default flags zero; libc may add SA_RESTORER on installation.
        // Keep that raw ABI observation in the audit, without masking any flag
        // from the subsequent exact comparison of the established fixture state.
        if (next_sigaction(watched[index], NULL, &raw) != 0 ||
            next_sigaction(watched[index], &raw, NULL) != 0 ||
            next_sigaction(watched[index], NULL, &original_actions[index]) != 0) _exit(96);
        initialized_action(watched[index], &raw, &original_actions[index]);
    }
}

/* Preserve the committed structured/raw bytes before any injected error.
 * These private copies let Python verify that restoration never rewrites facts.
 */
static void copy_committed(int record, int output, const char *source,
                           const char *destination, int optional) {
    int input = openat(record, source, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (input < 0 && optional && errno == ENOENT) return;
    if (input < 0) _exit(96);
    struct stat before, after;
    if (fstat(input, &before) != 0 || !S_ISREG(before.st_mode)) _exit(96);
    int copy = openat(output, destination,
                      O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (copy < 0) _exit(96);
    char buffer[4096];
    for (;;) {
        ssize_t count;
        do { count = read(input, buffer, sizeof(buffer)); } while (count < 0 && errno == EINTR);
        if (count < 0) _exit(96);
        if (count == 0) break;
        ssize_t offset = 0;
        while (offset < count) {
            ssize_t written = write(copy, buffer + offset, (size_t)(count - offset));
            if (written < 0 && errno == EINTR) continue;
            if (written <= 0) _exit(96);
            offset += written;
        }
    }
    if (fstat(input, &after) != 0 || before.st_dev != after.st_dev ||
        before.st_ino != after.st_ino || before.st_size != after.st_size ||
        before.st_mtim.tv_sec != after.st_mtim.tv_sec ||
        before.st_mtim.tv_nsec != after.st_mtim.tv_nsec) _exit(96);
    if (close(input) != 0 || close(copy) != 0) _exit(96);
}

int renameat(int old_dir, const char *old_name, int new_dir, const char *new_name) {
    resolve_functions();
    int result = next_renameat(old_dir, old_name, new_dir, new_name);
    int saved_errno = errno;
    if (result == 0 && is_owner() && strcmp(new_name, "finalized.json") == 0) {
        if (published) _exit(96);
        int output = open(snapshot_path, O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
        if (output < 0) _exit(96);
        copy_committed(new_dir, output, "metadata.json", "metadata.json", 0);
        copy_committed(new_dir, output, "events.jsonl", "events.jsonl", 0);
        copy_committed(new_dir, output, "finalized.json", "finalized.json", 0);
        copy_committed(new_dir, output, "raw/trace.log", "trace.log", 0);
        copy_committed(new_dir, output, "raw-integrity.json", "raw-integrity.json", 1);
        if (close(output) != 0) _exit(96);
        published = 1;
        audit("published", "recorder");
    }
    errno = saved_errno;
    return result;
}

int sigaction(int number, const struct sigaction *action, struct sigaction *old) {
    resolve_functions();
    if (is_owner() && published && (selected_mode == 3 || !injected) &&
        (selected_mode == 1 || selected_mode == 3) && action != NULL &&
        (number == SIGHUP || number == SIGINT || number == SIGTERM)) {
        injected += 1;
        audit("injected-sigaction", "recorder");
        errno = EIO;
        return -1;
    }
    return next_sigaction(number, action, old);
}

int sigprocmask(int how, const sigset_t *set, sigset_t *old) {
    resolve_functions();
    if (is_owner() && published && (selected_mode == 4 || !injected) &&
        (selected_mode == 2 || selected_mode == 4) &&
        how == SIG_SETMASK && set != NULL) {
        injected += 1;
        audit("injected-sigprocmask", "recorder");
        errno = EIO;
        return -1;
    }
    int result = next_sigprocmask(how, set, old);
    int saved_errno = errno;
    if (result == 0 && is_owner() && published && how == SIG_SETMASK && set != NULL)
        audit("restored-sigprocmask", "recorder");
    errno = saved_errno;
    return result;
}

/* pthread_sigmask returns an error number, unlike sigprocmask's -1/errno.
 * Both share one latch, but the regression asserts the actual sigprocmask path;
 * this hook does not claim coverage of an API that production does not call.
 */
int pthread_sigmask(int how, const sigset_t *set, sigset_t *old) {
    resolve_functions();
    if (is_owner() && published && (selected_mode == 4 || !injected) &&
        (selected_mode == 2 || selected_mode == 4) &&
        how == SIG_SETMASK && set != NULL) {
        int saved_errno = errno;
        injected += 1;
        audit("injected-pthread_sigmask", "recorder");
        errno = saved_errno;
        return EIO;
    }
    return next_pthread_sigmask(how, set, old);
}

__attribute__((destructor)) static void verify(void) {
    if (!is_owner() || !published) return;
    sigset_t mask;
    if (next_sigprocmask(SIG_SETMASK, NULL, &mask) != 0) _exit(96);
    if (selected_mode == 3 || selected_mode == 4) {
        /* This finite fixture starts with neutral, unblocked controls. The
         * explicit restore and Drop each try once; all controls must remain
         * blocked after persistent failure, with every other mask bit preserved.
         * Refusal is not proof of action mismatch when temporary/original DFL
         * actions coincide, so record that observation independently. */
        for (int number = 1; number < NSIG; ++number) {
            int control = number == SIGHUP || number == SIGINT || number == SIGTERM;
            int original = sigismember(&original_mask, number);
            if (original < 0 || (control && original != 0) ||
                sigismember(&mask, number) != (control ? 1 : original)) _exit(96);
        }
        int actions_match = 1;
        for (size_t index = 0; index < 3; ++index) {
            struct sigaction action;
            if (next_sigaction(watched[index], NULL, &action) != 0) _exit(96);
            actions_match &= action.sa_handler == original_actions[index].sa_handler;
            actions_match &= action.sa_flags == original_actions[index].sa_flags;
            for (int number = 1; number < NSIG; ++number)
                actions_match &= sigismember(&action.sa_mask, number) ==
                                 sigismember(&original_actions[index].sa_mask, number);
        }
        if (selected_mode == 4 && !actions_match) _exit(96);
        if (injected != (selected_mode == 3 ? 6 : 2)) _exit(96);
        audit(actions_match ? "actions-match-original" : "actions-differ-original", "recorder");
        audit("state-unrestored-controls-blocked", "recorder");
        return;
    }
    for (int number = 1; number < NSIG; ++number) {
        if (sigismember(&mask, number) != sigismember(&original_mask, number)) {
            audit("mask-mismatch", "recorder");
            _exit(96);
        }
    }
    // Query and compare defined fields in this same Linux process, including
    // libc's normalized flags (which can include SA_RESTORER), never padding.
    for (size_t index = 0; index < 3; ++index) {
        struct sigaction action;
        if (next_sigaction(watched[index], NULL, &action) != 0) _exit(96);
        if (action.sa_handler != original_actions[index].sa_handler ||
            action.sa_flags != original_actions[index].sa_flags) {
            action_mismatch(watched[index], &action, &original_actions[index]);
            _exit(96);
        }
        for (int number = 1; number < NSIG; ++number) {
            if (sigismember(&action.sa_mask, number) !=
                sigismember(&original_actions[index].sa_mask, number)) _exit(96);
        }
    }
    if (injected != (selected_mode != 0)) _exit(96);
    audit("state-restored", "recorder");
}
