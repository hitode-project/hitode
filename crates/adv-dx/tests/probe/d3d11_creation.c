/* Windows hardware and WARP oracle.
 * Build: i686-w64-mingw32-gcc -O1 -Wall -o d3d11_creation.exe d3d11_creation.c -ld3d11
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>

static void create(const char *label, D3D_DRIVER_TYPE type, UINT flags, const D3D_FEATURE_LEVEL *levels,
    UINT count, UINT sdk, BOOL device, BOOL immediate)
{
    ID3D11Device *out = (void *)0x1234;
    ID3D11DeviceContext *context = (void *)0x1234;
    D3D_FEATURE_LEVEL chosen = (D3D_FEATURE_LEVEL)0x5555;
    HRESULT hr = D3D11CreateDevice(NULL, type, NULL, flags, levels, count, sdk,
                                   device ? &out : NULL, &chosen, immediate ? &context : NULL);
    printf("create\t%s\t%08lx\t%04u\t%04x\t%s\t%s\n", label, (unsigned long)hr, sdk, (unsigned)chosen,
        out == NULL ? "null" : (out == (void *)0x1234 ? "unchanged" : "set"),
        context == NULL ? "null" : (context == (void *)0x1234 ? "unchanged" : "set"));
    if (hr >= 0 && immediate && context) ID3D11DeviceContext_Release(context);
    if (hr >= 0 && device && out) ID3D11Device_Release(out);
}

int main(void)
{
    D3D_FEATURE_LEVEL request[] = {0x9100, 0xb000, 0xb080, 0xc000, 0xb100};
    for (unsigned warp = 0; warp < 2; ++warp) {
        D3D_DRIVER_TYPE type = warp ? D3D_DRIVER_TYPE_WARP : D3D_DRIVER_TYPE_HARDWARE;
        printf("driver\t%s\n", warp ? "warp" : "hw");
        create("array-order", type, 0, request, 2, 7, 1, 1);
        create("invalid-after-valid", type, 0, request+1, 2, 7, 1, 1);
        create("invalid-before-valid", type, 0, request+2, 2, 7, 1, 1);
        create("sdk-zero", type, 0, NULL, 0, 0, 1, 1);
        create("sdk-minus", type, 0, NULL, 0, 6, 1, 1);
        create("sdk-plus", type, 0, NULL, 0, 8, 1, 1);
        create("null-levels-with-count", type, 0, NULL, 4, 7, 1, 1);
        create("empty-levels", type, 0, request, 0, 7, 1, 1);
        create("none-out", type, 0, NULL, 0, 7, 0, 0);
        create("context-out", type, 0, NULL, 0, 7, 0, 1);
        create("debug", type, 2, NULL, 0, 7, 1, 1);
    }
}
