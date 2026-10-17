#define _GNU_SOURCE
/* Mutate only the old image's new, unsealed request before owner exec. The
 * clean image never loads this module. This provides real admission controls
 * without exposing a production diagnostic selector. */
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
static int armed,request=-1;
static const char *mode,*audit;
static int (*next_memfd)(const char *,unsigned);
static int (*next_fcntl)(int,int,...);
static int (*next_execve)(const char *,char *const[],char *const[]);
static unsigned long long length_at(const unsigned char *bytes) {
 unsigned long long value=0;
 for(unsigned i=0;i<8;i++)value|=(unsigned long long)bytes[i]<<(8*i);
 return value;
}
static void remove_command(int fd,unsigned char *header) {
 size_t total=(size_t)length_at(header+16);
 unsigned char *bytes=malloc(total);if(!bytes||pread(fd,bytes,total,0)!=(ssize_t)total)_exit(96);
 unsigned selected=0,argc=0;for(unsigned i=0;i<4;i++) {
  selected|=(unsigned)header[64+i]<<(8*i);argc|=(unsigned)header[60+i]<<(8*i);
 }
 size_t at=72;
 for(unsigned i=0;i<2+selected;i++){if(at+8>total)_exit(96);at+=8+(size_t)length_at(bytes+at);if(at>total)_exit(96);}
 size_t start=at;
 for(unsigned i=0;i<argc;i++){if(at+8>total)_exit(96);at+=8+(size_t)length_at(bytes+at);if(at>total)_exit(96);}
 memmove(bytes+start,bytes+at,total-at);total-=at-start;
 memset(header+60,0,4);for(unsigned i=0;i<8;i++)header[16+i]=(unsigned char)((unsigned long long)total>>(8*i));
 memcpy(bytes,header,72);
 if(pwrite(fd,bytes,total,0)!=(ssize_t)total||ftruncate(fd,(off_t)total))_exit(96);
 free(bytes);
}
static void event(const char *name,int fd,int result,int error) {
 FILE *f=fopen(audit,"a");
 if(!f||fprintf(f,"event=%s fd=%d result=%d errno=%d\n",name,fd,result,error)<0||fclose(f))_exit(96);
}
__attribute__((constructor)) static void initialize(void) {
 const char *wanted=getenv("RD_REQUEST_RECORDER");mode=getenv("RD_REQUEST_MODE");audit=getenv("RD_REQUEST_AUDIT");
 char image[PATH_MAX];ssize_t size=readlink("/proc/self/exe",image,sizeof(image)-1);
 if(!wanted||!mode||!audit||size<0)return;
 image[size]=0;if(strcmp(image,wanted))return;
 next_memfd=dlsym(RTLD_NEXT,"memfd_create");next_fcntl=dlsym(RTLD_NEXT,"fcntl");next_execve=dlsym(RTLD_NEXT,"execve");
 if(!next_memfd||!next_fcntl||!next_execve)_exit(96);
 if(!strcmp(mode,"chld-no-cld-wait")) {
  struct sigaction ignored;memset(&ignored,0,sizeof(ignored));
  ignored.sa_handler=SIG_IGN;ignored.sa_flags=SA_NOCLDWAIT;
  if(sigaction(SIGCHLD,&ignored,NULL))_exit(96);
  struct sigaction observed;if(sigaction(SIGCHLD,NULL,&observed))_exit(96);
  event("chld-before-owner-exec",SIGCHLD,!!(observed.sa_flags&SA_NOCLDWAIT),0);
 }
 armed=1;
}
int memfd_create(const char *name,unsigned flags) {
 if(!next_memfd)next_memfd=dlsym(RTLD_NEXT,"memfd_create");
 if(!next_memfd)_exit(96);
 if(armed&&!strncmp(mode,"low-",4)) {
  int low=atoi(mode+4);if(low<0||low>2||close(low))_exit(96);
 }
 int fd;
 if(armed&&!strcmp(mode,"linked")) {
  const char *path=getenv("RD_REQUEST_LINKED");if(!path)_exit(96);
  /* Deliberately pass this owned linked descriptor across exec: CLOEXEC would
   * exercise EBADF admission instead of the promised nlink check. */
  fd=open(path,O_RDWR|O_CREAT|O_EXCL,0600);
 } else fd=next_memfd(name,flags);
 if(armed){request=fd;event("memfd-created",fd,fd,fd<0?errno:0);}
 return fd;
}
int fcntl(int fd,int command,...) {
 if(!next_fcntl)next_fcntl=dlsym(RTLD_NEXT,"fcntl");
 if(!next_fcntl)_exit(96);
 int value=0;
 if(command==F_DUPFD||command==F_DUPFD_CLOEXEC||command==F_SETFD||command==F_SETFL||command==F_ADD_SEALS) {
  va_list arguments;va_start(arguments,command);value=va_arg(arguments,int);va_end(arguments);
 }
 if(armed&&fd==request&&command==F_DUPFD) {
  int result=next_fcntl(fd,command,value);event("request-duplicated",fd,result,result<0?errno:0);request=result;return result;
 }
 if(armed&&fd==request&&command==F_ADD_SEALS) {
  if(!strncmp(mode,"low-",4)) {
   int low=atoi(mode+4);errno=0;int state=next_fcntl(low,F_GETFD);int error=errno;
   event("low-slot-after-dup",low,state,error);
  }
  unsigned char header[72];if(pread(fd,header,sizeof(header),0)!=(ssize_t)sizeof(header))_exit(96);
  if(!strcmp(mode,"magic"))header[0]=0;
  else if(!strcmp(mode,"version"))header[8]=2;
  else if(!strcmp(mode,"contract"))header[24]=255;
  else if(!strcmp(mode,"flags"))header[25]=128;
  else if(!strcmp(mode,"reserved"))header[26]=1;
  else if(!strcmp(mode,"zero-command"))remove_command(fd,header);
  else if(!strcmp(mode,"counts"))memset(header+60,255,4);
  else if(!strcmp(mode,"total"))memset(header+16,255,8);
  else if(!strcmp(mode,"length")) {unsigned char huge[8];memset(huge,255,8);if(pwrite(fd,huge,8,72)!=8)_exit(96);}
  else if(!strcmp(mode,"truncated")) {if(ftruncate(fd,71))_exit(96);}
  else if(!strcmp(mode,"offset")) {if(lseek(fd,0,SEEK_END)<0)_exit(96);}
  if(strcmp(mode,"truncated")&&pwrite(fd,header,sizeof(header),0)!=(ssize_t)sizeof(header))_exit(96);
  if(!strcmp(mode,"unsealed"))value&=~F_SEAL_WRITE;
  event("request-before-seal",fd,0,0);
  if(!strcmp(mode,"linked")) {
   event("false-seal-ack",fd,0,0);
   return 0; /* Deliberately broken old adapter; fresh fstat must reject it. */
  }
 }
 return next_fcntl(fd,command,value);
}
int execve(const char *path,char *const argv[],char *const env[]) {
 if(!next_execve)next_execve=dlsym(RTLD_NEXT,"execve");
 if(!next_execve)_exit(96);
 if(armed&&!strcmp(path,"/proc/self/exe")) {
  if(!strcmp(mode,"closed-fd")){if(close(request))_exit(96);}
  char *replacement[5]={argv[0],argv[1],argv[2],NULL,NULL};
  if(!strcmp(mode,"stdio-fd"))replacement[2]="2";
  else if(!strcmp(mode,"negative-fd"))replacement[2]="-1";
  else if(!strcmp(mode,"overflow-fd"))replacement[2]="99999999999999999999999999";
  else if(!strcmp(mode,"native-fd"))replacement[2]="\xff";
  else if(!strcmp(mode,"missing-fd"))replacement[2]=NULL;
  else if(!strcmp(mode,"extra-argument"))replacement[3]="extra";
  if(!strcmp(mode,"linked")) {
   errno=0;int state=next_fcntl(request,F_GETFD);int error=errno;
   event("linked-descriptor-flags",request,state,error);
  }
  event("owner-exec",request,0,0);
  return next_execve(path,replacement,env);
 }
 return next_execve(path,argv,env);
}
