#define _GNU_SOURCE
/* Host-image characterization only. The new recorder image must never load
 * this module, while its backend/target must retain the original environment.
 * The worker is created either in initialization or immediately before the
 * real owner exec, so success cannot be inferred from an uncalled old hook. */
#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
static pthread_mutex_t lock=PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t condition=PTHREAD_COND_INITIALIZER;
static pthread_t thread;
static int ready,owner,late,reaper;
static const char *report;
static int (*next_execve)(const char *,char *const[],char *const[]);
static void *worker(void *unused) {
 (void)unused;
 if(pthread_mutex_lock(&lock))_exit(96);
 ready=1;if(pthread_cond_signal(&condition))_exit(96);
 for(;;)if(pthread_cond_wait(&condition,&lock))_exit(96);
 return NULL;
}
static void start(void) {
 if(pthread_mutex_lock(&lock)||pthread_create(&thread,NULL,worker,NULL))_exit(96);
 while(!ready)if(pthread_cond_wait(&condition,&lock))_exit(96);
 if(pthread_mutex_unlock(&lock))_exit(96);
}
static int tasks(void) {
 DIR *d=opendir("/proc/self/task");if(!d)_exit(96);
 int n=0;struct dirent *e;while((e=readdir(d)))if(e->d_name[0]!='.')n++;
 if(closedir(d))_exit(96);
 return n;
}
__attribute__((constructor)) static void initialize(void) {
 const char *requested=getenv("RD_THREAD_GUARD_RECORDER");
 char image[PATH_MAX];ssize_t n=readlink("/proc/self/exe",image,sizeof(image)-1);
 if(!requested||n<0)return;
 image[n]=0;
 const char *log=getenv("RD_HOST_CTORS");
 if(log) {
  FILE *f=fopen(log,"a");if(!f||fprintf(f,"pid=%ld image=%s\n",(long)getpid(),image)<0||fclose(f))_exit(96);
 }
 if(strcmp(image,requested))return;
 owner=1;report=getenv("RD_THREAD_GUARD_REPORT");if(!report)_exit(96);
 const char *phase=getenv("RD_THREAD_GUARD_PHASE");
 late=phase&&strcmp(phase,"initial");reaper=phase&&!strcmp(phase,"reaper-gap");
 if(!late)start();
 next_execve=dlsym(RTLD_NEXT,"execve");if(!next_execve)_exit(96);
}
int execve(const char *path,char *const argv[],char *const env[]) {
 if(!next_execve)next_execve=dlsym(RTLD_NEXT,"execve");
 if(!next_execve)_exit(96);
 if(owner&&!strcmp(path,"/proc/self/exe")) {
  if(late)start();
  if(reaper) {
   struct sigaction ignored;memset(&ignored,0,sizeof(ignored));ignored.sa_handler=SIG_IGN;
   if(sigaction(SIGCHLD,&ignored,NULL))_exit(96);
   /* The host owns no collector child before this boundary. This reaper
    * attempt is recorded; its thread and disposition cannot enter the image. */
   (void)waitpid(-1,NULL,WNOHANG);
  }
  FILE *f=fopen(report,"wx");
  if(!f||fprintf(f,"owner=%ld tasks=%d worker=%d reaper=%d\n",(long)getpid(),tasks(),ready,reaper)<0||fclose(f))_exit(96);
 }
 return next_execve(path,argv,env);
}
