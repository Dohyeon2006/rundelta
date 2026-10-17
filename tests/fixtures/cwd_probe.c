/* Synchronization is through inherited pipes; coordinator is outside strace. */
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 3 || chdir(argv[1])) return 2;
    if (!strcmp(argv[2], "failed-chdir") && chdir("missing") != -1) return 3;
    if (!strcmp(argv[2], "failed-fchdir") && fchdir(-1) != -1) return 4;
    puts("CWD_READY"); fflush(stdout);
    char byte;
    if (read(0, &byte, 1) != 1 || byte != 'G') return 5;
    int fd = syscall(SYS_open, "file", O_RDONLY, 0);
    if (fd < 0) return 6;
    close(fd);
    if (syscall(SYS_access, "file", F_OK)) return 7;
    puts("ACCESS_OK");
    return 0;
}
