/* Wine 11 comparison oracle corpus; entirely authored input.
 * Build: i686-w64-mingw32-gcc -O2 -o nls_compare.exe nls_compare.c
 */
#define _WIN32_WINNT 0x0600
#include <windows.h>
#include <stdio.h>
#include <stdint.h>
#include <wchar.h>
#include <string.h>

static uint32_t seed = 0x735a2d91;
static uint32_t random32(void) { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; return seed; }
static const WCHAR alphabet[] = {
    'a','A','z','Z','0','1','9',' ', '-', '.', '/', 0, 0x301,0x308,0x30a,
    0xe4,0xe9,0xdf,0xc6,0x152,0x1c4,0xfb03,0x00ad,0x200b,0x00b7,
    0x3042,0x3041,0x304b,0x3099,0x309d,0x30a2,0x30a1,0x30ab,0x30fc,0x30fd,0xff71,0xff9e,
    0x4e00,0x4e01,0x3400,0x9fff,0x1100,0x115f,0x1160,0x1161,0x11a8,0xa960,0xd7b0,0xac00,0xd7a3,
    0x0660,0x0669,0xff10,0xff19,0x20ac,0x2603,0xd800,0xdc00,0xe000,0xf8ff,0xffff
};
static const DWORD bits[] = {1,2,4,8,0x10,0x20,0x1000,0x10000,0x20000,0x8000000,0x10000000,0x40000000};
static unsigned count;
static void result(int value) {
    DWORD error = GetLastError();
    ++count;
    printf("%d %08lx\n",value,(unsigned long)error);
}
#define CALL(expr) do { SetLastError(0x13579); result(expr); } while (0)
static BOOL CALLBACK compare_locale_entry(WCHAR *text) {
    LCID id=(LCID)wcstoul(text,NULL,16);
    const WCHAR *a=L"I\x00c6" L"ch-\x00e9" L"a";
    const WCHAR *b=L"\x0131" L"AEcH-eA";
    CALL(CompareStringW(id,0,a,-1,b,-1));
    CALL(CompareStringW(id,1,a,-1,b,-1));
    return TRUE;
}
int main(void) {
    const LCID locales[] = {0x411,0x804,0x404,0x412,0x409,0x7f,0x400,0x800};
    const WCHAR *names[] = {L"ja-JP",L"zh-CN",L"zh-TW",L"ko-KR",L"en-US",L"",NULL,L"!x-sys-default-locale"};
    static WCHAR a[1024], b[1024];
    static char x[1024], y[1024];
    for (unsigned l = 0; l < 8; ++l) {
        SetThreadLocale(locales[l]);
        for (unsigned c = 0; c < 4096; ++c) {
            DWORD flags = 0;
            for (unsigned bit = 0; bit < 12; ++bit) if (c & (1u << bit)) flags |= bits[bit];
            unsigned len = 1 + random32() % 48;
            if (c % 127 == 0) len = 700;
            for (unsigned i = 0; i < len; ++i) a[i] = random32() % 4 ? alphabet[random32() % (sizeof(alphabet)/sizeof(*alphabet))] : (WCHAR)random32();
            a[len] = 0;
            memcpy(b, a, (len + 1) * 2);
            if (c % 3) b[c % 3 == 1 ? len - 1 : random32() % len] = alphabet[random32() % (sizeof(alphabet)/sizeof(*alphabet))];
            int n1 = c % 7 == 0 ? -1 : c % 31 == 0 ? 0 : (int)len;
            int n2 = c % 11 == 0 ? -1 : (int)len;
            CALL(CompareStringW(locales[l], flags, a, n1, b, n2));
            CALL(CompareStringEx(names[l], flags, a, n1, b, n2, NULL, NULL, 0));
            CALL(CompareStringOrdinal(a, n1, b, n2, c % 4 == 0 ? 256 : c & 1));
            if (c % 16 == 0) {
                CALL(lstrcmpW(a,b)); CALL(lstrcmpiW(a,b));
            }
            for (unsigned i = 0; i < len; ++i) x[i] = (char)random32();
            x[len] = 0;
            memcpy(y, x, len + 1);
            if (c % 3) y[len - 1] ^= 0x27;
            CALL(CompareStringA(locales[l], flags & ~8u, x, n1, y, n2));
            if (c % 16 == 0) { CALL(lstrcmpA(x,y)); CALL(lstrcmpiA(x,y)); }
        }
        for (unsigned flags = 0; flags < 4; ++flags) {
            const DWORD f[] = {0, 0x40, 8, 0xffffffff};
            for (unsigned nulls = 0; nulls < 4; ++nulls) {
                const WCHAR *s = nulls & 1 ? NULL : a, *t = nulls & 2 ? NULL : b;
                CALL(CompareStringW(locales[l],f[flags],s,-1,t,-1));
                CALL(CompareStringA(locales[l],f[flags],(const char*)s,0,(const char*)t,0));
                CALL(CompareStringEx(names[l],f[flags],s,0,t,0,NULL,NULL,0));
                CALL(CompareStringOrdinal(s,0,t,0,TRUE));
                CALL(lstrcmpW(s,t)); CALL(lstrcmpiW(s,t));
                CALL(lstrcmpA((const char*)s,(const char*)t)); CALL(lstrcmpiA((const char*)s,(const char*)t));
            }
        }
    }
    for (unsigned i=0; i<1024; ++i) { a[i]='a'; b[i]='a'; }
    for (unsigned c=0;c<4096;++c) {
        unsigned len = 140 + random32()%880;
        a[len]=b[len]=0;
        if (c&1) a[len-1]=b[len-1]=0x301;
        b[len-2]=c%3 ? 'A' : 'a';
        CALL(CompareStringW(0x40c,c&1?0:1,a,len,b,len));
        CALL(CompareStringA(0x411,c&1?0:1,(char*)a,len,(char*)b,len));
        a[len-1]=b[len-1]='a'; a[len]=b[len]='a'; b[len-2]='a';
    }
    /* UTF-8 locales, reverse diacritics and linguistic compression tables. */
    const LCID extra[] = {0x439,0x41e,0x40c,0x405,0x40e,0x40a};
    for (unsigned l=0;l<sizeof(extra)/sizeof(*extra);++l) {
        for (unsigned c=0;c<1024;++c) {
            unsigned len=1+random32()%700;
            for (unsigned i=0;i<len;++i) {
                x[i]=(char)random32();
                a[i]=c&1 ? L"chdzsCSae\x0301\x0308"[random32()%11] : (WCHAR)random32();
            }
            x[len]=0; a[len]=0;
            memcpy(y,x,len+1); memcpy(b,a,(len+1)*2);
            if (c%3) { y[len-1]^=0x27; b[len-1]^=0x27; }
            CALL(CompareStringA(extra[l],c%8,x,len,y,len));
            CALL(CompareStringW(extra[l],c%8,a,len,b,len));
        }
    }
    /* Long secondary keys: case, expansions, reverse diacritics, kana and punctuation. */
    const LCID key_locales[] = {0x409,0x40c,0xc0c,0x411};
    const DWORD key_flags[] = {0,1,2,0x10,0x20,0x10000,0x20000};
    for (unsigned variant=0;variant<5;++variant) for (unsigned c=0;c<64;++c) {
        unsigned len=130+c*13;
        for (unsigned i=0;i<len;++i) {
            a[i]=variant==0 ? 'A' : variant==1 ? 0xc6 : variant==2 ? 0xe9 : variant==3 ? 0x3042 : (i&1 ? '-' : 'A');
            b[i]=a[i];
        }
        b[len-1]=variant==0 ? 'a' : variant==1 ? 0xe6 : variant==2 ? 'e' : variant==3 ? 0x30a2 : '\'';
        if (variant==2 && c&1) a[0]='e';
        a[len]=b[len]=0;
        for (unsigned l=0;l<4;++l) for (unsigned f=0;f<7;++f)
            CALL(CompareStringW(key_locales[l],key_flags[f],a,len,b,len));
    }
    /* Explicit lone DBCS leads, embedded NULs, and negative lengths. */
    for (unsigned l=0;l<8;++l) for (unsigned c=0;c<256;++c) {
        x[0]=y[0]=(char)c; x[1]=y[1]=0; y[2]='a'; x[2]='A'; x[3]=y[3]=0;
        CALL(CompareStringA(locales[l],0,x,1,y,1));
        CALL(CompareStringA(locales[l],0,x,3,y,3));
        CALL(CompareStringA(locales[l],1,x,-2,y,-1));
    }
    for (unsigned c=0;c<64;++c) {
        CALL(CompareStringW(0xdeadbeef,c&1?0:0x40,a,0,b,0));
        CALL(CompareStringEx(L"invalid-locale",c&1?0:0x40,a,0,b,0,NULL,NULL,0));
        CALL(CompareStringA(0xdeadbeef,c&1?0:0x40,x,0,y,0));
    }
    /* Identical strings cannot bypass any Wine flag or locale validation. */
    for (unsigned l=0;l<8;++l) for (unsigned c=0;c<4096;++c) {
        DWORD flags=0;
        for (unsigned bit=0;bit<12;++bit) if(c&(1u<<bit)) flags|=bits[bit];
        CALL(CompareStringW(locales[l],flags,L"a\0b",3,L"a\0b",3));
        CALL(CompareStringEx(names[l],flags,L"a\0b",3,L"a\0b",3,NULL,NULL,0));
        CALL(CompareStringA(locales[l],flags,"a\0b",3,"a\0b",3));
        CALL(CompareStringOrdinal(L"a\0b",3,L"a\0b",3,flags));
    }
    const WCHAR *ends[][2] = {
        {L"a\x0301z",L"a\x0302y"}, {L"a\x0301",L"\x00e1"},
        {L"a\x0301z",L"az"}, {L"a\x0301-",L"a-\x0301"},
        {L"\x304b\x3099",L"\x304c"}, {L"\x30ab\x3099",L"\x30ac"},
        {L"\x30cf\x309a",L"\x30d1"}, {L"\xff76\xff9e",L"\x30ac"},
        {L"\xff8a\xff9f",L"\x30d1"}, {L"\x3042\x309d",L"\x3042\x3042"},
        {L"\x00c6z",L"AEz"}, {L"\xfb03z",L"ffiz"}, {L"\x0152z",L"OEz"},
        {L"chz",L"cHz"}, {L"dzsz",L"dZsz"}, {L"csz",L"cSz"},
        {L"a-b",L"ab-"}, {L"a'b",L"ab'"}, {L"a b",L"ab"},
        {L"a\x00ad" L"b",L"ab"}, {L"a\x200b" L"b",L"ab"},
        {L"\x3042",L"\x30a2"}, {L"A",L"\xff21"}, {L"\xff71",L"\x30a2"},
        {L"\x00e9",L"e"}, {L"A",L"a"}, {L"a--",L"a.."},
        {L"a\x0301\x0308",L"a\x0308\x0301"}, {L"a\0Z",L"a\0Y"}
    };
    const DWORD shortcut_flags[] = {0,1,2,4,8,0x10,0x20,0x11,0x22,0x1000,0x10000,0x20000,0x30000,0x30003,0x30024,0x10000000,0x08000001};
    const LCID shortcut_locales[] = {0x411,0x409,0x40c,0xc0c,0x405,0x40e,0x40a};
    const unsigned prefixes[] = {0,1,2,15,129,257,768};
    for(unsigned pair=0;pair<sizeof(ends)/sizeof(*ends);++pair)
      for(unsigned p=0;p<sizeof(prefixes)/sizeof(*prefixes);++p) {
        unsigned prefix=prefixes[p];
        for(unsigned i=0;i<prefix;++i) a[i]=b[i]=L"Abcd X"[i%6];
        unsigned alen=pair==28?3:lstrlenW(ends[pair][0]), blen=pair==28?3:lstrlenW(ends[pair][1]);
        memcpy(a+prefix,ends[pair][0],alen*2); memcpy(b+prefix,ends[pair][1],blen*2);
        a[prefix+alen]=b[prefix+blen]=0;
        for(unsigned l=0;l<7;++l) for(unsigned f=0;f<sizeof(shortcut_flags)/sizeof(*shortcut_flags);++f) {
          CALL(CompareStringW(shortcut_locales[l],shortcut_flags[f],a,prefix+alen,b,prefix+blen));
          CALL(CompareStringW(shortcut_locales[l],shortcut_flags[f],a,-1,b,-1));
          int ac=WideCharToMultiByte(shortcut_locales[l]==0x411?932:1252,0,a,prefix+alen,x,1024,NULL,NULL);
          int bc=WideCharToMultiByte(shortcut_locales[l]==0x411?932:1252,0,b,prefix+blen,y,1024,NULL,NULL);
          x[ac]=y[bc]=0;
          CALL(CompareStringA(shortcut_locales[l],shortcut_flags[f],x,ac,y,bc));
        }
      }
    const DWORD invalid_flags[] = {0,0x40,0x100,0x80000000,0xffffffff};
    const LCID invalid_locales[] = {0xdeadbeef,0xffff,0x10000409};
    for(unsigned l=0;l<3;++l) for(unsigned f=0;f<5;++f) {
      CALL(CompareStringW(invalid_locales[l],invalid_flags[f],L"same",-1,L"same",-1));
      CALL(CompareStringA(invalid_locales[l],invalid_flags[f],"same",-1,"same",-1));
      CALL(CompareStringEx(L"invalid-locale",invalid_flags[f],L"same",-1,L"same",-1,NULL,NULL,0));
    }
    const int lengths[] = {-2,-1,0,1,4};
    for(unsigned i=0;i<5;++i) for(unsigned j=0;j<5;++j) for(unsigned f=0;f<5;++f) {
      CALL(CompareStringW(0x409,invalid_flags[f],NULL,lengths[i],L"same",lengths[j]));
      CALL(CompareStringW(0xdeadbeef,invalid_flags[f],L"same",lengths[i],NULL,lengths[j]));
      CALL(CompareStringA(0x409,invalid_flags[f],NULL,lengths[i],"same",lengths[j]));
    }
    static WCHAR long_a[40004],long_b[40004];
    const unsigned long_prefixes[] = {32765,32766,32767,32768,39900};
    for(unsigned p=0;p<5;++p) {
      unsigned prefix=long_prefixes[p];
      for(unsigned i=0;i<prefix;++i) long_a[i]=long_b[i]='A';
      memcpy(long_a+prefix,L"a-b",8); memcpy(long_b+prefix,L"ab-",8);
      for(unsigned f=0;f<4;++f) CALL(CompareStringW(0x409,f,long_a,prefix+3,long_b,prefix+3));
    }
    if (!EnumSystemLocalesW(compare_locale_entry,LCID_SUPPORTED)) return 2;
    printf("done %u\n",count);
    return 0;
}
