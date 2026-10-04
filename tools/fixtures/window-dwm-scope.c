#include "win32_min.h"
API BOOL WINAPI PeekMessageA(MSG *, HWND, UINT, UINT, UINT);
API DWORD WINAPI GetQueueStatus(UINT);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
static DWORD records[1024],words,mode,seen,suppress;
typedef struct { MSG msg; DWORD private_value; } NativeMsg;
static void record(DWORD tag,DWORD value){records[words++]=tag;records[words++]=value;}
static LRESULT WINAPI procedure(HWND window,UINT message,WPARAM wp,LPARAM lp){
 if(!suppress){
  if(message==0x81){seen|=1;if(mode==5)return 0;}
  if(message==1){seen|=2;if(mode==6)return -1;}
  if(message==2)seen|=4;
  if(message==0x82)seen|=8;
 }
 return DefWindowProcA(window,message,wp,lp);
}
static void drain(void){NativeMsg message={0};while(PeekMessageA(&message.msg,0,0,0,1)){} }
void mainCRTStartup(void){
 WNDCLASSA wc={0};NativeMsg message={0};HWND window,parent,old;DWORD i,tag,style,output,wrote;HANDLE file;
 wc.procedure=procedure;wc.instance=GetModuleHandleA(0);wc.name="AuthoredDwmScope";
 if(!RegisterClassA(&wc))ExitProcess(90);
 for(i=0;i<11;i++){
  mode=i;tag=(i+1)*100;parent=0;drain();GetQueueStatus(0x4ff);seen=0;
  style=i==1?0:i==2?0x80000000:i==3?0x40000000:0x02cb0000;
  if(i==8)style=0x80c80000;
  if(i==9)style=0x80800000;
  if(i==10)style=0x80040000;
  if(i==3){
   suppress=1;parent=CreateWindowExA(0x08000080,wc.name,"parent",0x02cb0000,-32000,-32000,64,64,0,0,wc.instance,0);
   suppress=0;drain();GetQueueStatus(0x4ff);seen=0;
  }
  if(i==4){parent=(HWND)-3;style=0;}
  window=CreateWindowExA(0x08000080,wc.name,"authored scope",style,-32000,-32000,32,32,parent,0,wc.instance,0);
  record(tag+1,window!=0);record(tag+2,seen);
  if(i==7){
   old=window;DestroyWindow(old);
   record(tag+10,GetQueueStatus(0x4ff));record(tag+11,PeekMessageA(&message.msg,0,0,0,1));
   record(tag+12,message.msg.message);
   window=CreateWindowExA(0x08000080,wc.name,"recreated",style,-32000,-32000,32,32,0,0,wc.instance,0);
   record(tag+13,window!=0);
  }
  record(tag+3,GetQueueStatus(0x4ff));
  output=PeekMessageA(&message.msg,0,0,0,1);record(tag+4,output);
  record(tag+5,output?message.msg.message:0);record(tag+6,output?message.msg.wparam:0);
  record(tag+7,output?(DWORD)message.msg.lparam:0);record(tag+8,output&&message.msg.hwnd==window);
  record(tag+9,PeekMessageA(&message.msg,0,0,0,1));
  if(window)DestroyWindow(window);if(i==3&&parent)DestroyWindow(parent);
 }
 record(9999,0x44574d53);
 file=CreateFileA("window-dwm-scope.bin",0x40000000,0,0,2,0,0);
 if(file==(HANDLE)-1||!WriteFile(file,records,words*4,&wrote,0)||wrote!=words*4)ExitProcess(91);
 CloseHandle(file);ExitProcess(0);
}
