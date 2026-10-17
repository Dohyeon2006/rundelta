#define _GNU_SOURCE
/* Acceptance tracer for its explicitly forked subject only. It does not attach to
 * arbitrary PIDs, change Yama/seccomp policies, or create a product test seam. */
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/fs.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>
#include <time.h>
#if !defined(__linux__) || !defined(__x86_64__)
#error Private syscall-register controller requires Linux x86_64
#endif
/* Addresses are read only from stopped, kernel-owned tracees. Directory
 * mutations below operate solely on the caller's new test storage. */
static int text_at(pid_t child,unsigned long address,char *out,size_t limit) {
 for(size_t at=0;at<limit;at+=sizeof(long)) {
  errno=0;long word=ptrace(PTRACE_PEEKDATA,child,(void *)(address+at),NULL);
  if(word==-1&&errno)return -1;
  size_t count=limit-at<sizeof(word)?limit-at:sizeof(word);
  memcpy(out+at,&word,count);
  if(memchr(out+at,0,count))return 0;
 }
 return -1;
}
static int directory_fd(pid_t child,int fd) {
 char path[128];int n=snprintf(path,sizeof(path),"/proc/%ld/fd/%d",(long)child,fd);
 if(n<0||(size_t)n>=sizeof(path))return -1;
 return open(path,O_RDONLY|O_DIRECTORY|O_CLOEXEC);
}
static int save_ready(const char *path,pid_t owner,pid_t child) {
 size_t length=strlen(path);
 if(length>SIZE_MAX-sizeof(".tmp.XXXXXX")){errno=ENAMETOOLONG;return -1;}
 char *temporary=malloc(length+sizeof(".tmp.XXXXXX"));if(!temporary)return -1;
 memcpy(temporary,path,length);memcpy(temporary+length,".tmp.XXXXXX",sizeof(".tmp.XXXXXX"));
 int descriptor=mkstemp(temporary);if(descriptor<0){free(temporary);return -1;}
 FILE *stream=fdopen(descriptor,"w");int result=0;
 if(!stream){int saved=errno;close(descriptor);unlink(temporary);free(temporary);errno=saved;return -1;}
 if(fprintf(stream,"owner=%ld child=%ld\n",(long)owner,(long)child)<0)result=-1;
 if(fflush(stream))result=-1;
 if(fclose(stream))result=-1;
 if(!result&&renameat2(AT_FDCWD,temporary,AT_FDCWD,path,RENAME_NOREPLACE))result=-1;
 int saved=errno;
 if(result)unlink(temporary);
 free(temporary);errno=saved;return result;
}
static int audit_stage(const char *event,int result,int error,nlink_t links,
                       const char *temporary,const char *retained) {
 const char *path=getenv("CAPTURE_FAULT_AUDIT");if(!path)return -1;
 FILE *f=fopen(path,"a");if(!f)return -1;
 int code=fprintf(f,"event=%s result=%d errno=%d links=%lu temporary=%s retained=%s\n",
                  event,result,error,(unsigned long)links,temporary,retained)<0?-1:0;
 if(fclose(f))code=-1;
 return code;
}
static int release_gate(const char *path) {
 int fd=open(path,O_RDONLY|O_CLOEXEC);if(fd<0)return -1;
 char byte;ssize_t n;do{n=read(fd,&byte,1);}while(n<0&&errno==EINTR);
 int result=n==1?0:-1;if(close(fd))result=-1;return result;
}
static int admitted(pid_t child) {
 char path[128],bytes[256];int n=snprintf(path,sizeof(path),"/proc/%ld/cmdline",(long)child);
 if(n<0||(size_t)n>=sizeof(path))return -1;
 int fd=open(path,O_RDONLY|O_CLOEXEC);if(fd<0)return -1;
 ssize_t count=read(fd,bytes,sizeof(bytes));int saved=errno;close(fd);errno=saved;
 if(count<0)return -1;
 static const char entry[]="__rundelta_capture_owner_v1";
 for(ssize_t at=0;at+(ssize_t)sizeof(entry)<=count;at++)
  if(!memcmp(bytes+at,entry,sizeof(entry)))return 1;
 return 0;
}
static int committed(const char *root) {
 char runs[4096];int n=snprintf(runs,sizeof(runs),"%s/runs",root);if(n<0||(size_t)n>=sizeof(runs))return -1;
 DIR *d=opendir(runs);if(!d)return errno==ENOENT?0:-1;
 int found=0;struct dirent *e;while((e=readdir(d))) {
  if(e->d_name[0]=='.')continue;
  char marker[4096];n=snprintf(marker,sizeof(marker),"%s/%s/finalized.json",runs,e->d_name);
  if(n<0||(size_t)n>=sizeof(marker)){closedir(d);return -1;}
  struct stat s;if(!lstat(marker,&s)&&S_ISREG(s.st_mode))found=1;
 }
 if(closedir(d))return -1;
 return found;
}

static int read_words(pid_t child,unsigned long address,unsigned long *out,size_t count) {
 if(!address)return 0;
 for(size_t i=0;i<count;i++){
  errno=0;long word=ptrace(PTRACE_PEEKDATA,child,(void *)(address+i*sizeof(long)),NULL);
  if(word==-1&&errno)return -1;
  out[i]=(unsigned long)word;
 }
 return 1;
}
static int signals(pid_t child,FILE *log,const char *stage) {
 char path[128];int n=snprintf(path,sizeof(path),"/proc/%ld/status",(long)child);
 if(n<0||(size_t)n>=sizeof(path))return -1;
 FILE *f=fopen(path,"r");if(!f)return -1;
 char *line=NULL;size_t size=0;
 while(getline(&line,&size,f)>=0)if(!strncmp(line,"SigBlk:",7)||!strncmp(line,"SigIgn:",7)||!strncmp(line,"SigCgt:",7)||!strncmp(line,"SigPnd:",7)||!strncmp(line,"ShdPnd:",7)){
  line[strcspn(line,"\n")]=0;fprintf(log,"event=signals stage=%s %s\n",stage,line);
 }
 free(line);return fclose(f);
}
static int copy_tree(const char *source,const char *target) {
 struct stat state;if(lstat(source,&state))return -1;
 if(S_ISREG(state.st_mode)){
  int in=open(source,O_RDONLY|O_NOFOLLOW|O_CLOEXEC);if(in<0)return -1;
  int out=open(target,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);if(out<0){close(in);return -1;}
  char buffer[4096];ssize_t count;
  while((count=read(in,buffer,sizeof(buffer)))!=0){
   if(count<0){if(errno==EINTR)continue;close(in);close(out);return -1;}
   for(ssize_t at=0;at<count;){ssize_t wrote=write(out,buffer+at,(size_t)(count-at));if(wrote<0&&errno==EINTR)continue;if(wrote<=0){close(in);close(out);return -1;}at+=wrote;}
  }
  int result=close(in);if(close(out))result=-1;return result;
 }
 if(!S_ISDIR(state.st_mode)||mkdir(target,0700))return -1;
 DIR *directory=opendir(source);if(!directory)return -1;struct dirent *entry;
 while((entry=readdir(directory))){
  if(!strcmp(entry->d_name,".")||!strcmp(entry->d_name,".."))continue;
  char from[4096],to[4096];int a=snprintf(from,sizeof(from),"%s/%s",source,entry->d_name),b=snprintf(to,sizeof(to),"%s/%s",target,entry->d_name);
  if(a<0||b<0||(size_t)a>=sizeof(from)||(size_t)b>=sizeof(to)||copy_tree(from,to)){closedir(directory);return -1;}
 }
 return closedir(directory);
}
static int snapshot_committed(const char *root,const char *events) {
 char source[4096],target[4096];int a=snprintf(source,sizeof(source),"%s/runs",root),b=snprintf(target,sizeof(target),"%s.committed-snapshot",events);
 if(a<0||b<0||(size_t)a>=sizeof(source)||(size_t)b>=sizeof(target))return -1;
 return copy_tree(source,target);
}
static int redirect(const char *path,int descriptor) {
 int fd=open(path,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,0600);if(fd<0)return -1;
 if(dup2(fd,descriptor)<0){close(fd);return -1;}
 return close(fd);
}
/* This scope follows only children identified by kernel fork events. A backend
 * is detached at its exec stop, before strace needs its own ptrace authority.
 * The broker becomes a subreaper solely for its test process tree. */
static int child_scope(const char *mode,const char *out,const char *err,
                       FILE *log,char **command) {
 if(prctl(PR_SET_CHILD_SUBREAPER,1,0,0,0))return 2;
 pid_t owner=fork();if(owner<0)return 2;
 if(!owner) {
  if(redirect(out,1)||redirect(err,2)||ptrace(PTRACE_TRACEME,0,NULL,NULL)||raise(SIGSTOP))_exit(125);
  execvp(command[0],command);_exit(126);
 }
 int status;if(waitpid(owner,&status,0)!=owner||!WIFSTOPPED(status))return 2;
 unsigned long options=PTRACE_O_TRACESYSGOOD|PTRACE_O_TRACEEXEC|PTRACE_O_EXITKILL|
                       PTRACE_O_TRACEFORK|PTRACE_O_TRACEVFORK|PTRACE_O_TRACECLONE;
 if(ptrace(PTRACE_SETOPTIONS,owner,NULL,(void *)options)||ptrace(PTRACE_SYSCALL,owner,NULL,NULL))return 2;
 struct child {pid_t pid;int denied,armed,held,deny_result,fd_replay,low_injected,low_observed;long watched;struct user_regs_struct saved;} children[16]={{0}};
 children[0].pid=owner;size_t count=1;int root_status=-1,denials=0,barriers=0;
 int owner_fd=(int)syscall(SYS_pidfd_open,owner,0),budget_kill=0,control_signal=0;
 if(owner_fd<0)return 2;
 struct timespec begun;if(clock_gettime(CLOCK_MONOTONIC,&begun))return 2;
 for(;;) {
  struct timespec now;if(clock_gettime(CLOCK_MONOTONIC,&now))return 2;
 double elapsed=(double)(now.tv_sec-begun.tv_sec)+(double)(now.tv_nsec-begun.tv_nsec)/1e9;
 if(!budget_kill&&root_status<0&&(!strncmp(mode,"pause-",6)||!strncmp(mode,"reap-",5)||!strncmp(mode,"cancel-",7)||!strcmp(mode,"version-read-eintr")||!strcmp(mode,"signal-read-eintr")||!strcmp(mode,"signal-poll-eintr"))&&elapsed>(!strcmp(mode,"pause-version")?12.0:(!strcmp(mode,"pause-formal")?8.0:14.0))) {
  budget_kill=1;fprintf(log,"event=controller-deadline-kill elapsed=%.9f\n",elapsed);fflush(log);
  if(syscall(SYS_pidfd_send_signal,owner_fd,SIGKILL,NULL,0))return 2;
 }
 if(!control_signal&&barriers&&!strcmp(mode,"cancel-version-wait-eintr")) {
  control_signal=1;fprintf(log,"event=controller-signal owner=%ld signal=15 elapsed=%.9f\n",(long)owner,elapsed);fflush(log);
  if(syscall(SYS_pidfd_send_signal,owner_fd,SIGTERM,NULL,0))return 2;
 }
 pid_t pid=waitpid(-1,&status,__WALL|WNOHANG);
 if(pid==0){usleep(1000);continue;}
 if(pid<0){if(errno==ECHILD)break;if(errno==EINTR)continue;return 2;}
  size_t slot=0;while(slot<count&&children[slot].pid!=pid)slot++;
  if(slot==count)return 2;
  struct child *state=&children[slot];
  if(WIFEXITED(status)||WIFSIGNALED(status)) {
   fprintf(log,"event=child-exit pid=%ld owner=%d raw_wait_status=%d signal=%d\n",
           (long)pid,pid==owner,status,WIFSIGNALED(status)?WTERMSIG(status):0);
   if(pid==owner) {
    root_status=status;
    for(size_t held=1;held<count;held++)if(children[held].held) {
     int fd=(int)syscall(SYS_pidfd_open,children[held].pid,0);
     if(fd<0&&errno!=ESRCH)return 2;
     if(fd>=0){if(syscall(SYS_pidfd_send_signal,fd,SIGKILL,NULL,0)&&errno!=ESRCH)return 2;close(fd);}
     if(ptrace(PTRACE_CONT,children[held].pid,NULL,NULL)&&errno!=ESRCH)return 2;
     children[held].held=0;
    }
   }
   continue;
  }
  if(!WIFSTOPPED(status))return 2;
  unsigned event=(unsigned)status>>16;int delivery=0;
  if(event==PTRACE_EVENT_FORK||event==PTRACE_EVENT_VFORK||event==PTRACE_EVENT_CLONE) {
   unsigned long next;if(ptrace(PTRACE_GETEVENTMSG,pid,NULL,&next)||count==16)return 2;
   children[count++]=(struct child){.pid=(pid_t)next,.watched=-1};
   fprintf(log,"event=kernel-owned-child parent=%ld child=%lu\n",(long)pid,next);
  } else if(event==PTRACE_EVENT_EXEC&&pid!=owner) {
   fprintf(log,"event=backend-detach pid=%ld\n",(long)pid);
   if(ptrace(PTRACE_DETACH,pid,NULL,NULL))return 2;
   continue;
  } else if(WSTOPSIG(status)==(SIGTRAP|0x80)) {
   struct __ptrace_syscall_info info;memset(&info,0,sizeof(info));
   if(ptrace(PTRACE_GET_SYSCALL_INFO,pid,(void *)sizeof(info),&info)<0)return 2;
   struct user_regs_struct regs;if(ptrace(PTRACE_GETREGS,pid,NULL,&regs))return 2;
   if(info.op==PTRACE_SYSCALL_INFO_ENTRY) {
    state->watched=(long)regs.orig_rax;
    if(pid==owner&&!strncmp(mode,"pipe-low-",9)&&!state->low_injected&&regs.orig_rax==SYS_pipe2) {
     int slot=mode[9]-'0';if(slot<0||slot>2||mode[10])return 2;
     state->saved=regs;state->fd_replay=1;
     regs.orig_rax=SYS_close;regs.rdi=(unsigned long long)slot;
     if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
    }
    if(pid==owner&&!strcmp(mode,"signal-read-eintr")&&count>=2&&regs.orig_rax==SYS_read&&regs.rdx==128) {
     char path[128],link[256];snprintf(path,sizeof(path),"/proc/%ld/fd/%llu",(long)pid,regs.rdi);
     ssize_t bytes=readlink(path,link,sizeof(link)-1);if(bytes<0)return 2;link[bytes]=0;
     if(!strcmp(link,"anon_inode:[signalfd]")) {
      fprintf(log,"event=signal-read-deny pid=%ld fd=%llu bytes=%llu errno=4 resource=%s\n",(long)pid,regs.rdi,regs.rdx,link);
      regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
      state->denied=1;state->deny_result=-EINTR;denials++;
     }
    }
    if(pid==owner&&!strcmp(mode,"signal-poll-eintr")&&count>=2&&regs.orig_rax==SYS_poll&&regs.rsi==5) {
     fprintf(log,"event=signal-poll-deny pid=%ld descriptors=%llu timeout=%lld errno=4\n",(long)pid,regs.rsi,(long long)regs.rdx);
     regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
     state->denied=1;state->deny_result=-EINTR;denials++;
    }
    if(pid==owner&&!strcmp(mode,"version-read-eintr")&&count>=2&&regs.orig_rax==SYS_read&&regs.rdx==8192) {
     char path[128],link[256];snprintf(path,sizeof(path),"/proc/%ld/fd/%llu",(long)pid,regs.rdi);
     ssize_t bytes=readlink(path,link,sizeof(link)-1);if(bytes<0)return 2;link[bytes]=0;
     if(!strncmp(link,"pipe:[",6)) {
      fprintf(log,"event=version-pipe-read-deny pid=%ld fd=%llu bytes=%llu errno=4 resource=%s\n",(long)pid,regs.rdi,regs.rdx,link);
      regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
      state->denied=1;state->deny_result=-EINTR;denials++;
     }
    }
    if(pid==owner&&regs.orig_rax==SYS_wait4&&(long long)regs.rdi>0)
     fprintf(log,"event=wait-entry pid=%ld child=%llu flags=%llu\n",(long)pid,regs.rdi,regs.rdx);
    if(pid==owner&&((!strcmp(mode,"reap-eintr")&&count>=3)||(!strcmp(mode,"cancel-version-wait-eintr")&&count>=2))&&regs.orig_rax==SYS_wait4&&(long long)regs.rdi>0) {
     fprintf(log,"event=wait-deny pid=%ld flags=%llu errno=4\n",(long)pid,regs.rdx);
     regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
     state->denied=1;state->deny_result=-EINTR;denials++;
    }
    if(pid==owner&&!strcmp(mode,"reap-zero-kill-deny")&&count>=3&&
       ((regs.orig_rax==SYS_wait4&&(long long)regs.rdi>0)||
        (regs.orig_rax==SYS_pidfd_send_signal&&regs.rsi==SIGKILL))) {
     state->deny_result=regs.orig_rax==SYS_wait4?0:-EPERM;
     fprintf(log,"event=cleanup-deny syscall=%llu flags_or_signal=%llu injected_return=%d\n",regs.orig_rax,regs.orig_rax==SYS_wait4?regs.rdx:regs.rsi,state->deny_result);
     regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
     state->denied=1;denials++;
    }
    if(pid!=owner&&state->armed&&!barriers&&regs.orig_rax==SYS_rt_sigaction&&regs.rdi==SIGHUP&&regs.rsi&&
       (((!strcmp(mode,"pause-version")||!strcmp(mode,"cancel-version-wait-eintr"))&&count==2)||(!strcmp(mode,"pause-formal")&&count==3))) {
     state->held=1;barriers++;fprintf(log,"event=async-restore-pause owner=%ld child=%ld phase=%s pdeath_armed=1\n",(long)owner,(long)pid,mode);fflush(log);
    }
    if(regs.orig_rax==SYS_prctl&&regs.rdi==PR_SET_PDEATHSIG&&regs.rsi==SIGKILL) {
     fprintf(log,"event=pdeath-arm-entry pid=%ld\n",(long)pid);
     if(!strcmp(mode,"prctl-deny")&&!denials) {
      regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
      state->denied=1;denials++;
     }
    }
    if(!strcmp(mode,"child-restore-barrier")&&state->armed&&!barriers&&
       regs.orig_rax==SYS_rt_sigaction&&regs.rdi==SIGHUP&&regs.rsi) {
     const char *ready=getenv("CAPTURE_FAULT_READY"),*release=getenv("CAPTURE_FAULT_RELEASE");
     if(!ready||!release||save_ready(ready,owner,pid))return 2;
     barriers++;fprintf(log,"event=restore-barrier owner=%ld child=%ld pdeath_armed=1\n",(long)owner,(long)pid);fflush(log);
     if(release_gate(release))return 2;
    }
   } else if(info.op==PTRACE_SYSCALL_INFO_EXIT) {
    if(state->fd_replay) {
     fprintf(log,"event=stdio-closed-before-pipe slot=%c actual_return=%lld\n",mode[9],(long long)info.exit.rval);
     regs=state->saved;regs.rax=SYS_pipe2;regs.orig_rax=SYS_pipe2;regs.rip-=2;
     if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;
     state->fd_replay=0;state->low_injected=1;
    } else if(pid==owner&&state->low_injected&&state->watched==SYS_pipe2&&info.exit.rval==0) {
     errno=0;long word=ptrace(PTRACE_PEEKDATA,pid,(void *)regs.rdi,NULL);if(word==-1&&errno)return 2;
     int descriptors[2];memcpy(descriptors,&word,sizeof(descriptors));
     fprintf(log,"event=actual-pipe-fds first=%d second=%d\n",descriptors[0],descriptors[1]);
     if(descriptors[0]==mode[9]-'0'||descriptors[1]==mode[9]-'0')state->low_observed=1;
    } else if(pid==owner&&state->low_observed&&state->watched==SYS_fcntl&&regs.rdi==(unsigned long long)(mode[9]-'0')&&regs.rsi==F_DUPFD_CLOEXEC)
     fprintf(log,"event=actual-high-dup old=%c lower_bound=%llu actual_return=%lld\n",mode[9],regs.rdx,(long long)info.exit.rval);
    else if(pid==owner&&state->low_observed&&state->watched==SYS_close&&regs.rdi==(unsigned long long)(mode[9]-'0'))
     fprintf(log,"event=actual-low-close slot=%c actual_return=%lld\n",mode[9],(long long)info.exit.rval);
    if(pid==owner&&state->watched==SYS_wait4)fprintf(log,"event=wait-exit pid=%ld actual_return=%lld injected=%d\n",(long)pid,(long long)info.exit.rval,state->denied);
    if(state->denied){regs.rax=(unsigned long long)(state->deny_result?state->deny_result:(!strcmp(mode,"reap-zero-kill-deny")?0:-EPERM));if(ptrace(PTRACE_SETREGS,pid,NULL,&regs))return 2;state->denied=0;}
    if(state->watched==SYS_prctl&&info.exit.rval==0) {
     state->armed=1;fprintf(log,"event=pdeath-arm-success pid=%ld return=0\n",(long)pid);
    }
   } else return 2;
  } else if(WSTOPSIG(status)!=SIGSTOP&&WSTOPSIG(status)!=SIGTRAP&&
            WSTOPSIG(status)!=(SIGTRAP|0x80))delivery=WSTOPSIG(status);
  if(state->held)continue;
  if(ptrace(PTRACE_SYSCALL,pid,NULL,(void *)(long)delivery)) {
   if(errno!=ESRCH)return 2;
  }
 }
 close(owner_fd);
 if(root_status<0)return 2;
 int code=WIFEXITED(root_status)?WEXITSTATUS(root_status):128+WTERMSIG(root_status);
 fprintf(log,"subject_returncode=%d raw_wait_status=%d denials=%d barriers=%d\n",code,root_status,denials,barriers);
 return code;
}
int main(int argc,char **argv) {
 if(argc==2&&!strcmp(argv[1],"_subject")) {
  errno=0;long value=syscall(SYS_getpid);int observed=errno;
  printf("actual_getpid=%ld actual_errno=%d\n",value,observed);
  return value==-1&&observed==EIO?0:1;
 }
 if(argc<8||strcmp(argv[6],"--"))return 2;
 const char *mode=argv[1],*root=argv[2],*out=argv[3],*err=argv[4],*events=argv[5];
 FILE *log=fopen(events,"wx");if(!log)return 2;
 if(!strcmp(mode,"prctl-deny")||!strcmp(mode,"child-restore-barrier")||!strcmp(mode,"pause-version")||!strcmp(mode,"pause-formal")||!strcmp(mode,"reap-eintr")||!strcmp(mode,"deadline-none")||!strcmp(mode,"cancel-version-wait-eintr")||!strcmp(mode,"reap-zero-kill-deny")||!strncmp(mode,"pipe-low-",9)||!strcmp(mode,"version-read-eintr")||!strcmp(mode,"signal-read-eintr")||!strcmp(mode,"signal-poll-eintr")) {
  int code=child_scope(mode,out,err,log,argv+7);if(fclose(log))return 2;return code;
 }
 pid_t child=fork();if(child<0)return 2;
 if(child==0) {
  if(redirect(out,STDOUT_FILENO)||redirect(err,STDERR_FILENO))_exit(124);
  if(ptrace(PTRACE_TRACEME,0,NULL,NULL)){perror("PTRACE_TRACEME refused");_exit(125);}
  if(raise(SIGSTOP))_exit(124);
  execvp(argv[7],argv+7);perror("exec subject failed");_exit(126);
 }
 int status;if(waitpid(child,&status,0)!=child)return 2;
 if(!WIFSTOPPED(status)) {
  fprintf(log,"initial_wait_status=%d trace_permission_verified=0\n",status);fclose(log);
  return WIFEXITED(status)?WEXITSTATUS(status):2;
 }
 if(ptrace(PTRACE_SETOPTIONS,child,NULL,(void *)(PTRACE_O_TRACESYSGOOD|PTRACE_O_TRACEEXEC|PTRACE_O_EXITKILL)))return 2;
 int injected=0,denials=0,executions=0,stops=0,snapshot=0,deliver=0,owner_image=0;
 int published_events=0,stage_armed=0,held_stage=-1,cleanup_watched=0;
 char temporary[256]={0},retained[300]={0};
 long watched_nr=-1;unsigned long watched_signal=0,watched_old=0;
 for(;;) {
  if(ptrace(PTRACE_SYSCALL,child,NULL,(void *)(long)deliver))return 2;
  deliver=0;
  if(waitpid(child,&status,0)!=child)return 2;
  if(WIFEXITED(status)||WIFSIGNALED(status))break;
  if(!WIFSTOPPED(status))return 2;
  stops++;
  if((unsigned)status>>16==PTRACE_EVENT_EXEC) {
   int admitted_image=admitted(child);if(admitted_image<0)return 2;
   owner_image|=admitted_image;
   executions++;fprintf(log,"event=exec execution=%d pid=%ld\n",executions,(long)child);continue;
  }
  int sig=WSTOPSIG(status);
  if(sig!=(SIGTRAP|0x80)) {
   if(sig==SIGTRAP||sig==SIGSTOP)continue;
   deliver=sig;continue;
  }
  struct __ptrace_syscall_info info;memset(&info,0,sizeof(info));
  long info_size=ptrace(PTRACE_GET_SYSCALL_INFO,child,(void *)sizeof(info),&info);
  if(info_size<0||(info.op!=PTRACE_SYSCALL_INFO_ENTRY&&info.op!=PTRACE_SYSCALL_INFO_EXIT)){fprintf(log,"event=syscall-info-unavailable errno=%d op=%d\n",errno,info.op);return 2;}
  int entering=info.op==PTRACE_SYSCALL_INFO_ENTRY;
  struct user_regs_struct regs;if(ptrace(PTRACE_GETREGS,child,NULL,&regs))return 2;
  if(entering&&info.entry.nr!=regs.orig_rax)return 2;
  if(entering) {
   watched_nr=-1;
   if(regs.orig_rax==SYS_wait4&&(long long)regs.rdi>0)
    fprintf(log,"event=owned-child-wait child=%llu owner=%ld\n",regs.rdi,(long)child);
   if(!strcmp(mode,"none")&&!snapshot&&regs.orig_rax==SYS_rt_sigaction&&regs.rsi) {
    int marker=committed(root);if(marker<0)return 2;
    if(marker){if(snapshot_committed(root,events))return 2;snapshot=1;fprintf(log,"event=committed-snapshot-control\n");}
   }
   if((regs.orig_rax==SYS_rt_sigaction&&(regs.rdi==SIGHUP||regs.rdi==SIGINT||regs.rdi==SIGTERM||regs.rdi==SIGCHLD))||regs.orig_rax==SYS_rt_sigprocmask){
    watched_nr=(long)regs.orig_rax;watched_signal=regs.rdi;watched_old=regs.rdx;
    unsigned long words[4]={0};size_t count=regs.orig_rax==SYS_rt_sigaction?4:1;int observed=read_words(child,regs.rsi,words,count);if(observed<0)return 2;
    fprintf(log,"event=signal-entry syscall=%ld selector=%lu nonnull=%d args=%016lx,%016lx,%016lx,%016lx old_pointer=%lu\n",watched_nr,watched_signal,observed,words[0],words[1],words[2],words[3],watched_old);
    if(signals(child,log,"entry"))return 2;
   }
   if(regs.orig_rax==SYS_exit_group&&signals(child,log,"before-exit"))return 2;
   int deny=!strcmp(mode,"getpid")&&regs.orig_rax==SYS_getpid&&denials==0;
   if((!strcmp(mode,"restore-actions")||!strcmp(mode,"one-action"))&&regs.orig_rax==SYS_rt_sigaction&&regs.rsi!=0&&
      (regs.rdi==SIGHUP||regs.rdi==SIGINT||regs.rdi==SIGTERM)) {
    int has_marker=committed(root);if(has_marker<0)return 2;deny=has_marker&&(!strcmp(mode,"restore-actions")||denials==0);
   }
   if(owner_image&&(!strcmp(mode,"init-action")||!strcmp(mode,"init-rollback")) && regs.orig_rax==SYS_rt_sigaction && regs.rsi!=0) {
    if(regs.rdi==SIGINT && denials==0) {
     int fd=(int)syscall(SYS_pidfd_open,child,0);if(fd<0)return 2;
     int sent=(int)syscall(SYS_pidfd_send_signal,fd,SIGHUP,NULL,0);int closed=close(fd);
     if(sent||closed)return 2;
     fprintf(log,"event=queued-pending-hup\n");deny=1;
    }
    if(!strcmp(mode,"init-rollback") && regs.rdi==SIGHUP && denials>0)deny=1;
   }
   if(owner_image&&regs.orig_rax==SYS_rt_sigaction&&regs.rdi==SIGCHLD&&regs.rsi) {
    int has_marker=committed(root);if(has_marker<0)return 2;
    if(!strcmp(mode,"init-chld")&&!has_marker&&!denials)deny=1;
    if(has_marker&&(!strcmp(mode,"restore-chld")||(!strcmp(mode,"one-chld")&&!denials)))deny=1;
   }
   if(regs.orig_rax==SYS_renameat) {
    char oldname[256],newname[256];
    if(text_at(child,regs.rsi,oldname,sizeof(oldname))||text_at(child,regs.r10,newname,sizeof(newname)))return 2;
    published_events=!strcmp(newname,"events.jsonl");
    if(!strcmp(mode,"finalize")&&!strcmp(newname,"finalized.json")&&getenv("CAPTURE_FAULT_FAIL"))deny=1;
    if(!stage_armed&&(!strcmp(mode,"stage-replace")||!strcmp(mode,"stage-swap")||!strcmp(mode,"stage-no-swap"))&&published_events) {
     if(strncmp(oldname,".rundelta-tmp-",14))return 2;
     strcpy(temporary,oldname);snprintf(retained,sizeof(retained),"%s-retained",oldname);
     if(!strcmp(mode,"stage-replace")) {
      int dir=directory_fd(child,(int)regs.rdi);const char *sentinel=getenv("CAPTURE_FAULT_SENTINEL");
      if(dir<0||!sentinel||unlinkat(dir,temporary,0)||linkat(AT_FDCWD,sentinel,dir,temporary,0)||close(dir))return 2;
      const char *name=getenv("CAPTURE_FAULT_NAME");FILE *f=name?fopen(name,"wx"):NULL;
      if(!f||fputs(temporary,f)==EOF||fclose(f))return 2;
     }
     if(audit_stage("rename-failed",-1,EIO,0,temporary,retained))return 2;
     stage_armed=1;deny=1;
    }
   }
   if(stage_armed&&regs.orig_rax==SYS_unlinkat&&regs.rdx==0&&strcmp(mode,"stage-replace")) {
    char name[256];if(text_at(child,regs.rsi,name,sizeof(name)))return 2;
    if(!strcmp(name,temporary)) {
     int dir=directory_fd(child,(int)regs.rdi);if(dir<0)return 2;
     held_stage=openat(dir,temporary,O_RDONLY|O_NOFOLLOW|O_CLOEXEC);if(held_stage<0)return 2;
     if(!strcmp(mode,"stage-swap")) {
      if(renameat(dir,temporary,dir,retained))return 2;
      int replacement=openat(dir,temporary,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);
      static const char bytes[]="replacement actually removed\n";
      if(replacement<0||write(replacement,bytes,sizeof(bytes)-1)!=(ssize_t)sizeof(bytes)-1||close(replacement))return 2;
     }
     if(close(dir))return 2;
     cleanup_watched=1;
    }
   }
   if((!strcmp(mode,"restore-mask")||!strcmp(mode,"one-mask"))&&regs.orig_rax==SYS_rt_sigprocmask&&regs.rdi==SIG_SETMASK&&regs.rsi!=0) {
    int has_marker=committed(root);if(has_marker<0)return 2;deny=has_marker&&(!strcmp(mode,"restore-mask")||denials==0);
   }
   if(deny) {
    if((!strncmp(mode,"restore-",8)||!strncmp(mode,"one-",4))&&!snapshot){if(snapshot_committed(root,events))return 2;snapshot=1;fprintf(log,"event=committed-snapshot-before-denial\n");}
    fprintf(log,"event=deny syscall=%lld signal=%llu after_marker=%d pid=%ld\n",(long long)regs.orig_rax,regs.rdi,(!strncmp(mode,"restore-",8)||!strncmp(mode,"one-",4)),(long)child);
    regs.orig_rax=(unsigned long long)-1;if(ptrace(PTRACE_SETREGS,child,NULL,&regs))return 2;
    injected=1;denials++;
   }
  } else {
   if(injected){fprintf(log,"event=kernel-invalid-syscall-return value=%lld actual_phase=exit\n",(long long)info.exit.rval);regs.rax=(unsigned long long)-EIO;if(ptrace(PTRACE_SETREGS,child,NULL,&regs))return 2;injected=0;}
   if(watched_nr!=-1){
    unsigned long words[4]={0};size_t count=watched_nr==SYS_rt_sigaction?4:1;int observed=read_words(child,watched_old,words,count);if(observed<0)return 2;
    fprintf(log,"event=signal-exit syscall=%ld selector=%lu return=%lld old_observed=%d old=%016lx,%016lx,%016lx,%016lx\n",watched_nr,watched_signal,(long long)regs.rax,observed,words[0],words[1],words[2],words[3]);
    if(signals(child,log,"exit"))return 2;
   }
   if(published_events&&regs.orig_rax==SYS_renameat&&info.exit.rval==0&&!strcmp(mode,"finalize")) {
    const char *ready=getenv("CAPTURE_FAULT_READY"),*release=getenv("CAPTURE_FAULT_RELEASE");
    if(!ready||!release||save_ready(ready,child,0))return 2;
    fprintf(log,"event=events-published-barrier owner=%ld\n",(long)child);fflush(log);
    if(release_gate(release))return 2;
   }
   if(cleanup_watched&&regs.orig_rax==SYS_unlinkat) {
    struct stat state;if(fstat(held_stage,&state)||close(held_stage))return 2;
    int result=(int)info.exit.rval;
    if(audit_stage("cleanup-unlink",result,result<0?-result:0,state.st_nlink,temporary,retained))return 2;
    held_stage=-1;cleanup_watched=0;stage_armed=0;
   }
  }
 }
 int code=WIFEXITED(status)?WEXITSTATUS(status):128+WTERMSIG(status);
 fprintf(log,"subject_returncode=%d raw_wait_status=%d denials=%d exec_events=%d syscall_stops=%d\n",code,status,denials,executions,stops);if(fclose(log))return 2;
 printf("subject_returncode=%d denials=%d exec_events=%d\n",code,denials,executions);
 return code;
}
