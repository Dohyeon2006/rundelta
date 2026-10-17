#define _GNU_SOURCE
/* Test-only, exact-image injection into freshly owned fixture stores. No scan or
 * process-name signaling; ordinary execution is unchanged without RD_DP_MODE. */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
static int (*real_move)(int,const char *,int,const char *,unsigned int);
static int (*real_unlink)(int,const char *,int);
static int (*real_sync)(int);
static const char *mode;
static const char *audit_path;
static pid_t owner;
static void log_line(const char *event,int rc,int error) {
    if (!audit_path) return;
    int fd=open(audit_path,O_WRONLY|O_CREAT|O_APPEND|O_NOFOLLOW|O_CLOEXEC,0600);
    if(fd<0) _exit(94);
    char text[256];int size=snprintf(text,sizeof text,"event=%s result=%d errno=%d\n",event,rc,error);
    if(size<0 || size>=(int)sizeof text || write(fd,text,(size_t)size)!=size || close(fd)) _exit(94);
}
__attribute__((constructor)) static void initialize(void) {
    real_move=dlsym(RTLD_NEXT,"renameat2");real_unlink=dlsym(RTLD_NEXT,"unlinkat");real_sync=dlsym(RTLD_NEXT,"fsync");
    if(!real_move||!real_unlink||!real_sync) _exit(94);
    const char *binary=getenv("RD_DP_BINARY");mode=getenv("RD_DP_MODE");audit_path=getenv("RD_DP_AUDIT");
    char image[4096];ssize_t length=readlink("/proc/self/exe",image,sizeof image-1);
    if(!binary||!mode||length<0) {mode=NULL;return;} image[length]='\0';
    if(strcmp(binary,image)) {mode=NULL;return;} owner=getpid();
}
int renameat2(int oldfd,const char *oldname,int newfd,const char *newname,unsigned flags) {
    int active=mode && getpid()==owner;
    if(active && !strcmp(newname,"record")) {
        if(!strcmp(mode,"isolation-fail")) {log_line("isolation-fail",-1,EIO);errno=EIO;return -1;}
        if(!strcmp(mode,"isolation-swap")) {
            if(renameat(oldfd,oldname,oldfd,"opened-original") || mkdirat(oldfd,oldname,0700)) _exit(94);
            int replacement=openat(oldfd,oldname,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
            int file=openat(replacement,"replacement",O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC,0600);
            if(replacement<0||file<0||write(file,"foreign",7)!=7||close(file)||close(replacement)) _exit(94);
            log_line("source-replaced",0,0);
        }
        int rc=real_move(oldfd,oldname,newfd,newname,flags);int error=errno;
        log_line("isolation-rename",rc,rc?error:0);
        if(!rc && !strcmp(mode,"commit-interrupt")) _exit(87);
        errno=error;return rc;
    }
    if(active && !strcmp(oldname,"record") && !strcmp(mode,"restore-fail")) {
        log_line("restore-fail",-1,EIO);errno=EIO;return -1;
    }
    return real_move(oldfd,oldname,newfd,newname,flags);
}
int fsync(int fd) {
    if(mode && getpid()==owner && !strcmp(mode,"intent-sync-fail")) {
        char link[64],path[4096];snprintf(link,sizeof link,"/proc/self/fd/%d",fd);
        ssize_t length=readlink(link,path,sizeof path-1);
        if(length>=0) {path[length]='\0';char *leaf=strrchr(path,'/');
            if(leaf && !strcmp(leaf+1,"purge-started")) {log_line("purge-intent-sync-fail",-1,EIO);errno=EIO;return -1;}}
    }
    return real_sync(fd);
}
int unlinkat(int fd,const char *name,int flags) {
    if(mode && getpid()==owner && !strcmp(mode,"purge-first-fail") && !flags) {
        log_line("first-unlink-fail",-1,EIO);errno=EIO;return -1;
    }
    if(mode && getpid()==owner && !strcmp(mode,"cleanup-dir-swap") && flags==AT_REMOVEDIR && !strcmp(name,"record")) {
        if(renameat(fd,name,fd,"opened-original") || mkdirat(fd,name,0700)) _exit(94);
        int rc=real_unlink(fd,name,flags);int error=errno;
        log_line("replacement-unlink",rc,rc?error:0);errno=error;return rc;
    }
    return real_unlink(fd,name,flags);
}
