#include <fcntl.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <signal.h>
int main(int argc, char **argv) {
    if (argc < 2 || !strcmp(argv[1], "ok")) return 0;
    if (!strcmp(argv[1], "signal")) { raise(SIGTERM); return 0; }
    if (!strcmp(argv[1], "wait")) { sleep(30); return 0; }
    if (!strcmp(argv[1], "child")) { pid_t p=fork(); if (!p) { execl(argv[0],argv[0],"ok",NULL); _exit(99); } int s; waitpid(p,&s,0); return WEXITSTATUS(s); }
    if (!strcmp(argv[1], "fchdir")) { int dir=open(argv[2],O_RDONLY|O_DIRECTORY); if(dir<0 || fchdir(dir))return 3; close(dir); int fd=open("relative.txt",O_RDONLY); if(fd<0)return 1;close(fd);return 0; }
    if (!strcmp(argv[1], "cwd")) { if(chdir(argv[2])) return 3; int fd=open("relative.txt",O_RDONLY); if(fd<0)return 1;close(fd);return 0; }
    const char *path=getenv("PROBE_CONFIG");
    int fd=open(path ? path : argv[1],O_RDONLY);
    if(fd<0)return 1; close(fd);return 0;
}
