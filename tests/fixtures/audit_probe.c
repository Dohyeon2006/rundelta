#define _GNU_SOURCE
/* Controlled targets for the acceptance audit; not part of the Rust product. */
#include <errno.h>
#include <fcntl.h>
#include <linux/fs.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int publish_ready(const char *path,pid_t first,pid_t second,int pair) {
    size_t length=strlen(path);
    if(length>SIZE_MAX-sizeof(".tmp.XXXXXX")){errno=ENAMETOOLONG;return -1;}
    char *temporary=malloc(length+sizeof(".tmp.XXXXXX"));if(!temporary)return -1;
    memcpy(temporary,path,length);memcpy(temporary+length,".tmp.XXXXXX",sizeof(".tmp.XXXXXX"));
    int descriptor=mkstemp(temporary);if(descriptor<0){free(temporary);return -1;}
    FILE *stream=fdopen(descriptor,"w");int result=0;
    if(!stream){int saved=errno;close(descriptor);unlink(temporary);free(temporary);errno=saved;return -1;}
    int written=pair?fprintf(stream,"%ld %ld\n",(long)first,(long)second):fprintf(stream,"%ld\n",(long)first);
    if(written<0)result=-1;
    if(fflush(stream))result=-1;
    if(fclose(stream))result=-1;
    if(!result&&renameat2(AT_FDCWD,temporary,AT_FDCWD,path,RENAME_NOREPLACE))result=-1;
    int saved=errno;if(result)unlink(temporary);free(temporary);errno=saved;return result;
}
static int read_relative(const char *p) {
    int fd=syscall(SYS_open,p,O_RDONLY,0);
    if(fd<0)return 7;
    close(fd);return 0;
}
int main(int argc,char **argv) {
    if(argc<2)return 2;
    if(!strcmp(argv[1],"ok"))return 0;
    if(!strcmp(argv[1],"exit143"))return 143;
    if(!strcmp(argv[1],"exit-code"))return atoi(argv[2]);
    if(!strcmp(argv[1],"signal")){raise(atoi(argv[2]));return 3;}
    if(!strcmp(argv[1],"realtime")){raise(SIGRTMIN);return 3;}
    if(!strcmp(argv[1],"tty")){
        if(!isatty(0) || !isatty(1))return 11;
        puts("TTY_READY");fflush(stdout);
        char line[64];if(!fgets(line,sizeof(line),stdin))return 12;
        printf("TTY_ECHO:%s",line);return 0;
    }
    if(!strcmp(argv[1],"wait-tree") || !strcmp(argv[1],"wait-detached")){
        int syncfd[2];if(pipe(syncfd))return 4;
        pid_t child=fork();if(child<0)return 4;
        if(!child){if(!strcmp(argv[1],"wait-detached") && setsid()<0)_exit(13);close(syncfd[0]);write(syncfd[1],"x",1);close(syncfd[1]);for(;;)pause();}
        close(syncfd[1]);char byte;read(syncfd[0],&byte,1);close(syncfd[0]);
        if(publish_ready(argv[2],getpid(),child,1)){
            kill(child,SIGKILL);waitpid(child,NULL,0);return 4;
        }
        for(;;)pause();
    }
    if(!strcmp(argv[1],"wait")){
        if(publish_ready(argv[2],getpid(),0,0))return 4;
        for(;;)pause();
    }
    if(!strcmp(argv[1],"cwd")){
        if(chdir(argv[2]) || chdir("parent"))return 4;
        pid_t p=fork();if(p<0)return 5;
        if(!p){
            if(chdir("../child"))_exit(6);
            if(read_relative("child.txt"))_exit(7);
            int fd=open("../fd",O_RDONLY|O_DIRECTORY);
            if(fd<0 || fchdir(fd))_exit(8);
            close(fd);_exit(read_relative("fd.txt"));
        }
        int s; if(waitpid(p,&s,0)<0 || !WIFEXITED(s) || WEXITSTATUS(s))return 9;
        if(chdir("does-not-exist")==0)return 10;
        return read_relative("parent.txt");
    }
    return 2;
}
