/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 d3d9_texture_stages.c -o d3d9_texture_stages.exe -ld3d9 */
/* Every stage-0 colour and alpha combination either fails at its setter or
 * draws the documented D3D9 result. d3d9-texture-stages.bin holds, per
 * combination, the setter results, the draw result and the read pixel.
 * Exit status: 0 when every accepted combination drew its expected pixel,
 * else 10 + the number of wrong combinations (at most 200). */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d9.h>

typedef struct Vertex {
    float x, y, z, rhw;
    DWORD color;
    float u, v;
} Vertex;

enum { EDGE = 16 };
static const DWORD TEXEL = 0x60c04080u;   /* a r g b */
static const DWORD DIFFUSE = 0x9080ff20u;
static const DWORD FACTOR = 0x30102040u;
static const DWORD OPS[] = {
    D3DTOP_SELECTARG1, D3DTOP_SELECTARG2, D3DTOP_MODULATE, D3DTOP_MODULATE2X,
    D3DTOP_MODULATE4X, D3DTOP_ADD, D3DTOP_SUBTRACT,
};
static const DWORD ARGS[] = {D3DTA_DIFFUSE, D3DTA_CURRENT, D3DTA_TEXTURE, D3DTA_TFACTOR};

static HANDLE out;
static void put(DWORD value) {
    DWORD n;
    if (!WriteFile(out, &value, 4, &n, NULL) || n != 4) ExitProcess(2);
}

static int channel(DWORD color, int shift) { return (int)(color >> shift) & 0xff; }

static int argument(DWORD arg, int shift) {
    switch (arg) {
    case D3DTA_TEXTURE: return channel(TEXEL, shift);
    case D3DTA_TFACTOR: return channel(FACTOR, shift);
    default: return channel(DIFFUSE, shift); /* DIFFUSE, and CURRENT at stage 0 */
    }
}

static int combine(DWORD op, int a, int b) {
    int v;
    switch (op) {
    case D3DTOP_SELECTARG1: return a;
    case D3DTOP_SELECTARG2: return b;
    case D3DTOP_MODULATE: return a * b / 255;
    case D3DTOP_MODULATE2X: v = 2 * a * b / 255; break;
    case D3DTOP_MODULATE4X: v = 4 * a * b / 255; break;
    case D3DTOP_ADD: v = a + b; break;
    default: v = a - b; return v < 0 ? 0 : v; /* SUBTRACT */
    }
    return v > 255 ? 255 : v;
}

static DWORD expected(const DWORD color[3], const DWORD alpha[3]) {
    DWORD pixel = (DWORD)combine(alpha[0], argument(alpha[1], 24), argument(alpha[2], 24)) << 24;
    int shift;
    for (shift = 0; shift <= 16; shift += 8)
        pixel |= (DWORD)combine(color[0], argument(color[1], shift), argument(color[2], shift)) << shift;
    return pixel;
}

static int close_to(DWORD a, DWORD b) {
    int shift;
    for (shift = 0; shift <= 24; shift += 8) {
        int d = channel(a, shift) - channel(b, shift);
        if (d < -2 || d > 2) return 0;
    }
    return 1;
}

int main(void) {
    HWND window = CreateWindowExA(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, "STATIC", "texture stages",
                                  WS_OVERLAPPED, -32000, -32000, 64, 64, NULL, NULL,
                                  GetModuleHandleA(NULL), NULL);
    IDirect3D9 *factory = Direct3DCreate9(D3D_SDK_VERSION);
    IDirect3DDevice9 *d = NULL;
    IDirect3DSurface9 *back = NULL, *read = NULL;
    IDirect3DTexture9 *texture = NULL;
    D3DPRESENT_PARAMETERS p = {0};
    D3DLOCKED_RECT lock;
    Vertex quad[4] = {
        {-.5f, -.5f, 0, 1, DIFFUSE, 0, 0},
        {EDGE - .5f, -.5f, 0, 1, DIFFUSE, 1, 0},
        {-.5f, EDGE - .5f, 0, 1, DIFFUSE, 0, 1},
        {EDGE - .5f, EDGE - .5f, 0, 1, DIFFUSE, 1, 1},
    };
    unsigned wrong = 0, which, i, j, k;

    if (!window || !factory) return 3;
    out = CreateFileA("d3d9-texture-stages.bin", GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                      FILE_ATTRIBUTE_NORMAL, NULL);
    if (out == INVALID_HANDLE_VALUE) return 4;
    p.BackBufferWidth = EDGE;
    p.BackBufferHeight = EDGE;
    p.BackBufferFormat = D3DFMT_A8R8G8B8;
    p.BackBufferCount = 1;
    p.SwapEffect = D3DSWAPEFFECT_DISCARD;
    p.hDeviceWindow = window;
    p.Windowed = TRUE;
    p.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    if (FAILED(IDirect3D9_CreateDevice(factory, 0, D3DDEVTYPE_HAL, window,
                                       D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE,
                                       &p, &d)))
        return 5;
    if (FAILED(IDirect3DDevice9_GetBackBuffer(d, 0, 0, D3DBACKBUFFER_TYPE_MONO, &back)) ||
        FAILED(IDirect3DDevice9_CreateOffscreenPlainSurface(d, EDGE, EDGE, D3DFMT_A8R8G8B8,
                                                            D3DPOOL_SYSTEMMEM, &read, NULL)) ||
        FAILED(IDirect3DDevice9_CreateTexture(d, 1, 1, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED,
                                              &texture, NULL)) ||
        FAILED(IDirect3DTexture9_LockRect(texture, 0, &lock, NULL, 0)))
        return 6;
    *(DWORD *)lock.pBits = TEXEL;
    IDirect3DTexture9_UnlockRect(texture, 0);
    IDirect3DDevice9_SetTexture(d, 0, (IDirect3DBaseTexture9 *)texture);
    IDirect3DDevice9_SetFVF(d, D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    IDirect3DDevice9_SetRenderState(d, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_ZENABLE, D3DZB_FALSE);
    IDirect3DDevice9_SetRenderState(d, D3DRS_TEXTUREFACTOR, FACTOR);
    IDirect3DDevice9_SetSamplerState(d, 0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetSamplerState(d, 0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    IDirect3DDevice9_SetTextureStageState(d, 1, D3DTSS_COLOROP, D3DTOP_DISABLE);

    /* which 0 varies the colour combination under a texture-alpha select,
     * which 1 the alpha combination under a texture-colour select. */
    for (which = 0; which < 2; ++which)
        for (i = 0; i < sizeof OPS / sizeof *OPS; ++i)
            for (j = 0; j < 4; ++j)
                for (k = 0; k < 4; ++k) {
                    DWORD fixed[3] = {D3DTOP_SELECTARG1, D3DTA_TEXTURE, D3DTA_DIFFUSE};
                    DWORD varied[3] = {OPS[i], ARGS[j], ARGS[k]};
                    const DWORD *color = which ? fixed : varied, *alpha = which ? varied : fixed;
                    HRESULT set[6], drawn = E_FAIL;
                    DWORD pixel = 0, state;
                    int accepted = 1;
                    set[0] = IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_COLOROP, color[0]);
                    set[1] = IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_COLORARG1, color[1]);
                    set[2] = IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_COLORARG2, color[2]);
                    set[3] = IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_ALPHAOP, alpha[0]);
                    set[4] = IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_ALPHAARG1, alpha[1]);
                    set[5] = IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_ALPHAARG2, alpha[2]);
                    for (state = 0; state < 6; ++state) {
                        put((DWORD)set[state]);
                        if (FAILED(set[state])) accepted = 0;
                    }
                    if (accepted) {
                        IDirect3DDevice9_Clear(d, 0, NULL, D3DCLEAR_TARGET, 0x01020304u, 1, 0);
                        IDirect3DDevice9_BeginScene(d);
                        drawn = IDirect3DDevice9_DrawPrimitiveUP(d, D3DPT_TRIANGLESTRIP, 2, quad,
                                                                 sizeof(Vertex));
                        IDirect3DDevice9_EndScene(d);
                        if (SUCCEEDED(IDirect3DDevice9_GetRenderTargetData(d, back, read)) &&
                            SUCCEEDED(IDirect3DSurface9_LockRect(read, &lock, NULL, D3DLOCK_READONLY))) {
                            pixel = *(DWORD *)((BYTE *)lock.pBits + EDGE / 2 * lock.Pitch + EDGE / 2 * 4);
                            IDirect3DSurface9_UnlockRect(read);
                        }
                        if (FAILED(drawn) || !close_to(pixel, expected(color, alpha))) ++wrong;
                    }
                    put((DWORD)drawn);
                    put(pixel);
                    /* The setters keep a rejected value out; restore a known state. */
                    IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
                    IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
                    IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_COLORARG2, D3DTA_CURRENT);
                    IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
                    IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
                    IDirect3DDevice9_SetTextureStageState(d, 0, D3DTSS_ALPHAARG2, D3DTA_CURRENT);
                }
    CloseHandle(out);
    return wrong ? 10 + (wrong > 200 ? 200 : wrong) : 0;
}
