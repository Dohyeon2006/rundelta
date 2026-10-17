
#define _GNU_SOURCE
/* Private acceptance interposer, never a production selector. Activation is
 * restricted to the exact requested executable and its current PID; inherited
 * copies in strace/target remain inactive. A refused events rename supplies the
 * primary failure. In swap mode, the original staging inode is renamed aside
 * immediately before the real cleanup unlink removes a fresh replacement.
 * Holding a second fd lets the observer report the original inode's link count
 * after that real operation. No restoration or atomic-exclusion claim follows
 * from detection. Missing observers terminate with 96 instead of passing. */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
static int (*rename_original)(int,const char *,int,const char *);
static int (*unlink_original)(int,const char *,int);
static pid_t owner;
static char temporary[128], retained[160];
static const char *mode, *audit_path;
static int armed;
__attribute__((constructor)) static void initialize(void) {
    rename_original=dlsym(RTLD_NEXT,"renameat");
    unlink_original=dlsym(RTLD_NEXT,"unlinkat");
    if (!rename_original || !unlink_original) _exit(96);
    const char *requested=getenv("RD_STAGE_BINARY");
    mode=getenv("RD_STAGE_MODE"); audit_path=getenv("RD_STAGE_AUDIT");
    char image[PATH_MAX]; ssize_t size=readlink("/proc/self/exe",image,sizeof(image)-1);
    if (!requested || !mode || !audit_path || size<0) return;
    image[size]='\0'; if (!strcmp(image,requested)) owner=getpid();
}
static void audit(const char *event, int result, int saved_errno, nlink_t links) {
    char line[512]; int length=snprintf(line,sizeof(line),"event=%s result=%d errno=%d links=%lu temporary=%s retained=%s\n",event,result,saved_errno,(unsigned long)links,temporary,retained);
    if (length<0 || (size_t)length>=sizeof(line)) _exit(96);
    int fd=open(audit_path,O_WRONLY|O_CREAT|O_APPEND|O_CLOEXEC|O_NOFOLLOW,0600);
    if (fd<0 || write(fd,line,(size_t)length)!=length || close(fd)!=0) _exit(96);
}
int renameat(int oldfd,const char *oldname,int newfd,const char *newname) {
    if (owner && getpid()==owner && !armed && strcmp(mode,"normal") && !strcmp(newname,"events.jsonl")) {
        if (strlen(oldname)>=sizeof(temporary)) _exit(96);
        strcpy(temporary,oldname); snprintf(retained,sizeof(retained),"%s-retained",oldname);
        armed=1; audit("rename-failed",-1,EIO,0); errno=EIO; return -1;
    }
    return rename_original(oldfd,oldname,newfd,newname);
}
int unlinkat(int dir,const char *name,int flags) {
    if (owner && getpid()==owner && armed && flags==0 && !strcmp(name,temporary)) {
        armed=0;
        int held=openat(dir,name,O_RDONLY|O_CLOEXEC|O_NOFOLLOW); struct stat before,after;
        if (held<0 || fstat(held,&before)!=0) _exit(96);
        if (!strcmp(mode,"swap")) {
            if (rename_original(dir,name,dir,retained)!=0) _exit(96);
            int replacement=openat(dir,name,O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC|O_NOFOLLOW,0600);
            static const char bytes[]="replacement actually removed\n";
            if (replacement<0 || write(replacement,bytes,sizeof(bytes)-1)!=(ssize_t)(sizeof(bytes)-1) || close(replacement)!=0) _exit(96);
        }
        int result=unlink_original(dir,name,flags); int saved=errno;
        if (fstat(held,&after)!=0 || close(held)!=0) _exit(96);
        audit("cleanup-unlink",result,result==0?0:saved,after.st_nlink);
        errno=saved; return result;
    }
    return unlink_original(dir,name,flags);
}
