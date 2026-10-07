/* Authored thread-local message metadata oracle; no native window/input. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
static DWORD words[32],count,worker_words[3];
static void record(DWORD value){words[count++]=value;}
static DWORD WINAPI worker(void *argument){(void)argument;worker_words[0]=(DWORD)GetMessageExtraInfo();worker_words[1]=(DWORD)SetMessageExtraInfo((LPARAM)0xfedcba98u);worker_words[2]=(DWORD)GetMessageExtraInfo();return 0;}
void mainCRTStartup(void){
    MSG message;HANDLE thread;DWORD wrote,i;
    record((DWORD)GetMessageExtraInfo());record((DWORD)SetMessageExtraInfo((LPARAM)0x12345678));record((DWORD)GetMessageExtraInfo());
    record(PeekMessageA(&message,NULL,0,0,PM_NOREMOVE));record((DWORD)GetMessageExtraInfo());
    if(!PostThreadMessageA(GetCurrentThreadId(),WM_APP,7,9))ExitProcess(2);
    record((DWORD)GetMessageExtraInfo());record(PeekMessageA(&message,NULL,0,0,PM_NOREMOVE));record((DWORD)GetMessageExtraInfo());
    record((DWORD)SetMessageExtraInfo((LPARAM)0x87654321u));record((DWORD)GetMessageExtraInfo());
    record(PeekMessageA(&message,NULL,0,0,PM_REMOVE));record((DWORD)GetMessageExtraInfo());
    record((DWORD)SetMessageExtraInfo((LPARAM)0x12345678));
    thread=CreateThread(NULL,0,worker,NULL,0,NULL);if(!thread)ExitProcess(3);
    if(WaitForSingleObject(thread,10000)!=WAIT_OBJECT_0)ExitProcess(4);CloseHandle(thread);
    for(i=0;i<3;i++)record(worker_words[i]);record((DWORD)GetMessageExtraInfo());
    record((DWORD)SetMessageExtraInfo(0));record((DWORD)GetMessageExtraInfo());record(0xe8710001);
    thread=CreateFileA("message-extra-info.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(thread==INVALID_HANDLE_VALUE)ExitProcess(90);if(!WriteFile(thread,words,count*4,&wrote,NULL)||wrote!=count*4)ExitProcess(91);CloseHandle(thread);ExitProcess(0);
}
