#define _GNU_SOURCE
/* Characterize the intentional loss of an old-image caught callback at exec.
 * The callback reports an actual invocation through its preopened private FD. */
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static int audit=-1;
static void handler(int number) {
 (void)number;
 static const char bytes[]="old-image-callback\n";
 if(write(audit,bytes,sizeof(bytes)-1)!=(ssize_t)sizeof(bytes)-1)_exit(96);
}
__attribute__((constructor)) static void initialize(void) {
 const char *wanted=getenv("RD_CAUGHT_RECORDER"),*path=getenv("RD_CAUGHT_AUDIT");
 char image[PATH_MAX];ssize_t size=readlink("/proc/self/exe",image,sizeof(image)-1);
 if(!wanted||!path||size<0)return;
 image[size]=0;if(strcmp(image,wanted))return;
 audit=open(path,O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC,0600);if(audit<0)_exit(96);
 static const char initial[]="old-image-initialized\n";
 if(write(audit,initial,sizeof(initial)-1)!=(ssize_t)sizeof(initial)-1)_exit(96);
 struct sigaction action;memset(&action,0,sizeof(action));action.sa_handler=handler;action.sa_flags=SA_RESTART;
 if(sigaction(SIGINT,&action,NULL))_exit(96);
}
