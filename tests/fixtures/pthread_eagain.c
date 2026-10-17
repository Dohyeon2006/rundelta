#define _GNU_SOURCE

#include <errno.h>
#include <pthread.h>

/* RF-430 regression fixture: any attempted relay-thread creation must fail
 * deterministically. The signalfd/pidfd supervisor does not call this symbol. */
int pthread_create(
    pthread_t *thread,
    const pthread_attr_t *attributes,
    void *(*start)(void *),
    void *argument
) {
    (void)thread;
    (void)attributes;
    (void)start;
    (void)argument;
    return EAGAIN;
}
