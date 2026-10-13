/*
 * Authored x86 kerning boundary oracle using installed Arial at height -32 in a private memory DC.
 * Inputs: null and bounded output buffers, sentinel bytes, and valid/invalid DCs.
 * Checks cover pair counts, partial copies, guard preservation, and LastError.
 * Run from the repository root: ./tools/fixtures/build-kerning-pairs-native.ps1
 * The LLVM/Windows SDK build script runs the probe and prints numeric observations.
 * Output: kerning-pairs-native.bin under target/kerning-pairs-native by default.
 * No window is created and no font is installed.
 */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
static DWORD values[64], used;
static void record(DWORD value) { values[used++] = value; }
static void query(HDC dc, DWORD capacity, KERNINGPAIR *output) {
    SetLastError(0x12345678);
    record(GetKerningPairsW(dc, capacity, output)); record(GetLastError());
}
void mainCRTStartup(void) {
    HDC dc = CreateCompatibleDC(NULL);
    HFONT font = CreateFontW(-32,0,0,0,400,0,0,0,DEFAULT_CHARSET,0,0,0,0,L"Arial");
    HFONT old;
    HANDLE output;
    DWORD written, count, i;
    KERNINGPAIR buffer[4];
    if (!dc || !font) ExitProcess(1);
    old = (HFONT)SelectObject(dc,font);
    count = GetKerningPairsW(dc,0,NULL);
    record(count);
    for(i=0;i<4;i++) { buffer[i].wFirst=0xcccc; buffer[i].wSecond=0xdddd; buffer[i].iKernAmount=0x12345678; }
    query(dc,0,NULL);
    query(dc,0xffffffff,NULL);
    query(dc,0,buffer); record(buffer[0].wFirst); record(buffer[0].wSecond); record(buffer[0].iKernAmount);
    query(dc,1,buffer); record(buffer[0].iKernAmount); record(buffer[1].iKernAmount);
    query(dc,4,buffer); record(buffer[3].iKernAmount);
    query(NULL,0,NULL);
    query((HDC)(DWORD_PTR)0xffffffff,0,NULL);
    SelectObject(dc,old); DeleteObject(font); DeleteDC(dc);
    output=CreateFileA("kerning-pairs-native.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(output==INVALID_HANDLE_VALUE || !WriteFile(output,values,used*4,&written,NULL) || written!=used*4) ExitProcess(2);
    CloseHandle(output); ExitProcess(0);
}
