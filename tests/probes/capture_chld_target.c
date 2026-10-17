#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
/* Read the actual target inheritance, after owner and backend exec. */
int main(int argc,char **argv) {
 if(argc==3&&!strcmp(argv[1],"launch")) {
  struct sigaction original;memset(&original,0,sizeof(original));
  original.sa_handler=SIG_IGN;original.sa_flags=SA_NOCLDWAIT;
  if(sigaction(SIGCHLD,&original,NULL))return 2;
  struct sigaction seen;if(sigaction(SIGCHLD,NULL,&seen))return 2;
  if(printf("before_exec ignored=%d no_cld_wait=%d\n",seen.sa_handler==SIG_IGN,
            !!(seen.sa_flags&SA_NOCLDWAIT))<0||fflush(stdout))return 2;
  char *command[]={argv[0],argv[2],NULL};execv(argv[0],command);return 2;
 }
 if(argc!=2)return 2;
 struct sigaction state;if(sigaction(SIGCHLD,NULL,&state))return 2;
 FILE *f=fopen(argv[1],"wx");if(!f)return 2;
 if(fprintf(f,"ignored=%d no_cld_wait=%d\n",state.sa_handler==SIG_IGN,
            !!(state.sa_flags&SA_NOCLDWAIT))<0)return 2;
 return fclose(f)?2:0;
}
