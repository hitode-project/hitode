/* IDirectInputDevice8::SetActionMap assigns each device object's app data only
 * from actions mapped to that device without DIA_APPNOMAP.
 * Build: i686-w64-mingw32-gcc -O2 -Wall -Wextra -o dinput_action_map.exe dinput_action_map.c -ldinput8 -ldxguid -lole32 -luuid */
#define DIRECTINPUT_VERSION 0x0800
#define CINTERFACE
#define COBJMACROS
#include <windows.h>
#include <dinput.h>
#include <stdio.h>
#include <string.h>

static unsigned failures;

static void check(const char *name, int passed)
{
    printf("%s=%s\n", name, passed ? "ok" : "FAIL");
    fflush(stdout);
    if (!passed) failures++;
}

static int get_app_data(IDirectInputDevice8W *dev, DWORD obj_id, UINT_PTR *out)
{
    DIPROPPOINTER prop;
    HRESULT hr;

    memset(&prop, 0, sizeof(prop));
    prop.diph.dwSize = sizeof(prop);
    prop.diph.dwHeaderSize = sizeof(prop.diph);
    prop.diph.dwObj = obj_id;
    prop.diph.dwHow = DIPH_BYID;
    hr = IDirectInputDevice8_GetProperty(dev, DIPROP_APPDATA, &prop.diph);
    if (FAILED(hr)) {
        printf("get_app_data(obj=%#lx) failed hr=%#lx\n", (unsigned long)obj_id, (unsigned long)hr);
        fflush(stdout);
        return 0;
    }
    *out = prop.uData;
    return 1;
}

static const GUID test_action_map_guid = {
    0x6b2f6c10, 0x3b8a, 0x4d92, {0x9e, 0x41, 0x77, 0x20, 0x91, 0x5c, 0xa8, 0x04}
};

static void init_action_format(DIACTIONFORMATW *af, DIACTIONW *actions, DWORD count)
{
    memset(af, 0, sizeof(*af));
    af->dwSize = sizeof(*af);
    af->dwActionSize = sizeof(DIACTIONW);
    af->dwDataSize = count * sizeof(DWORD);
    af->dwNumActions = count;
    af->rgoAction = actions;
    af->guidActionMap = test_action_map_guid;
    af->dwGenre = DIVIRTUAL_SPACESIM;
    af->dwBufferSize = 32;
    lstrcpyW(af->tszActionMap, L"Probe");
}

int main(void)
{
    IDirectInput8W *dinput = NULL;
    IDirectInputDevice8W *kbd = NULL;
    IDirectInputDevice8W *mouse = NULL;
    HRESULT hr;

    hr = DirectInput8Create(GetModuleHandleW(NULL), DIRECTINPUT_VERSION,
                            &IID_IDirectInput8W, (void **)&dinput, NULL);
    check("create_dinput", SUCCEEDED(hr) && dinput != NULL);
    if (FAILED(hr) || !dinput) return 1;

    hr = IDirectInput8_CreateDevice(dinput, &GUID_SysKeyboard, &kbd, NULL);
    check("create_keyboard", SUCCEEDED(hr) && kbd != NULL);
    hr = IDirectInput8_CreateDevice(dinput, &GUID_SysMouse, &mouse, NULL);
    check("create_mouse", SUCCEEDED(hr) && mouse != NULL);
    if (!kbd || !mouse) return 1;

    /* 1. Reuse one DIACTIONFORMATW across keyboard then mouse with DIDBAM_DEFAULT.
     * Keyboard keys 3 and 4 share dwObjID 0x404 and 0x504 with mouse buttons 1 and 2. */
    {
        DIACTIONW actions[5];
        DIACTIONFORMATW af;
        UINT_PTR v0 = 0, v1 = 0, v2 = 0, k3 = 0, k4 = 0;

        memset(actions, 0, sizeof(actions));
        actions[0].uAppData = 0x101;
        actions[0].dwSemantic = DIMOUSE_BUTTON0;
        actions[0].lptszActionName = L"M0";
        actions[1].uAppData = 0x102;
        actions[1].dwSemantic = DIMOUSE_BUTTON1;
        actions[1].lptszActionName = L"M1";
        actions[2].uAppData = 0x103;
        actions[2].dwSemantic = DIMOUSE_BUTTON2;
        actions[2].lptszActionName = L"M2";
        actions[3].uAppData = 0x201;
        actions[3].dwSemantic = DIKEYBOARD_3;
        actions[3].lptszActionName = L"K3";
        actions[4].uAppData = 0x202;
        actions[4].dwSemantic = DIKEYBOARD_4;
        actions[4].lptszActionName = L"K4";

        init_action_format(&af, actions, 5);
        hr = IDirectInputDevice8_BuildActionMap(kbd, &af, NULL, DIDBAM_DEFAULT);
        check("kbd_build", SUCCEEDED(hr));
        hr = IDirectInputDevice8_SetActionMap(kbd, &af, NULL, DIDSAM_DEFAULT);
        check("kbd_set", SUCCEEDED(hr));
        check("kbd_app_data",
              get_app_data(kbd, actions[3].dwObjID, &k3) && k3 == 0x201 &&
              get_app_data(kbd, actions[4].dwObjID, &k4) && k4 == 0x202);

        /* Reuse af directly on mouse without zeroing stale keyboard dwObjIDs. */
        hr = IDirectInputDevice8_BuildActionMap(mouse, &af, NULL, DIDBAM_DEFAULT);
        check("mouse_build_default", SUCCEEDED(hr));
        hr = IDirectInputDevice8_SetActionMap(mouse, &af, NULL, DIDSAM_DEFAULT);
        check("mouse_set_default", SUCCEEDED(hr));
        check("shared_default",
              get_app_data(mouse, actions[0].dwObjID, &v0) && v0 == 0x101 &&
              get_app_data(mouse, actions[1].dwObjID, &v1) && v1 == 0x102 &&
              get_app_data(mouse, actions[2].dwObjID, &v2) && v2 == 0x103);

        /* 2. Reuse with DIDBAM_PRESERVE so keyboard entries retain GUID_SysKeyboard. */
        for (int i = 0; i < 5; i++) {
            actions[i].guidInstance = GUID_NULL;
            actions[i].dwObjID = 0;
            actions[i].dwHow = 0;
        }
        hr = IDirectInputDevice8_BuildActionMap(kbd, &af, NULL, DIDBAM_DEFAULT);
        check("preserve_kbd_build", SUCCEEDED(hr));
        hr = IDirectInputDevice8_BuildActionMap(mouse, &af, NULL, DIDBAM_PRESERVE);
        check("preserve_mouse_build", SUCCEEDED(hr));
        hr = IDirectInputDevice8_SetActionMap(mouse, &af, NULL, DIDSAM_DEFAULT);
        check("preserve_mouse_set", SUCCEEDED(hr));
        check("shared_preserve",
              get_app_data(mouse, actions[0].dwObjID, &v0) && v0 == 0x101 &&
              get_app_data(mouse, actions[1].dwObjID, &v1) && v1 == 0x102 &&
              get_app_data(mouse, actions[2].dwObjID, &v2) && v2 == 0x103);

        /* 3. Later DIA_APPNOMAP entry with matching GUID and dwObjID must be ignored. */
        {
            DWORD btn0_id = actions[0].dwObjID;
            DWORD btn1_id = actions[1].dwObjID;
            DWORD btn2_id = actions[2].dwObjID;
            DIACTIONW nomap_actions[3];
            DIACTIONFORMATW nomap_af;

            memset(nomap_actions, 0, sizeof(nomap_actions));
            nomap_actions[0].uAppData = 0x101;
            nomap_actions[0].dwSemantic = DIMOUSE_BUTTON0;
            nomap_actions[0].guidInstance = GUID_SysMouse;
            nomap_actions[0].dwObjID = btn0_id;
            nomap_actions[0].dwHow = DIAH_DEFAULT;
            nomap_actions[0].lptszActionName = L"M0";

            nomap_actions[1].uAppData = 0x102;
            nomap_actions[1].dwSemantic = DIMOUSE_BUTTON1;
            nomap_actions[1].guidInstance = GUID_SysMouse;
            nomap_actions[1].dwObjID = btn1_id;
            nomap_actions[1].dwHow = DIAH_DEFAULT;
            nomap_actions[1].lptszActionName = L"M1";

            nomap_actions[2].uAppData = 0x999;
            nomap_actions[2].dwSemantic = DIMOUSE_BUTTON1;
            nomap_actions[2].dwFlags = DIA_APPNOMAP;
            nomap_actions[2].guidInstance = GUID_SysMouse;
            nomap_actions[2].dwObjID = btn1_id;
            nomap_actions[2].dwHow = DIAH_DEFAULT;
            nomap_actions[2].lptszActionName = L"NoMap";

            init_action_format(&nomap_af, nomap_actions, 3);
            hr = IDirectInputDevice8_SetActionMap(mouse, &nomap_af, NULL, DIDSAM_DEFAULT);
            check("nomap_set", SUCCEEDED(hr));
            check("appnomap_ignored",
                  get_app_data(mouse, btn0_id, &v0) && v0 == 0x101 &&
                  get_app_data(mouse, btn1_id, &v1) && v1 == 0x102);

            /* 4. Unmapped mouse objects reset to 0 even if another device or GUID_NULL shares dwObjID. */
            nomap_actions[1].uAppData = 0x888;
            nomap_actions[1].guidInstance = GUID_SysKeyboard;
            nomap_actions[1].dwFlags = 0;

            nomap_actions[2].uAppData = 0x777;
            nomap_actions[2].guidInstance = GUID_NULL;
            nomap_actions[2].dwObjID = btn2_id;
            nomap_actions[2].dwFlags = 0;

            hr = IDirectInputDevice8_SetActionMap(mouse, &nomap_af, NULL, DIDSAM_DEFAULT);
            check("unmapped_set", SUCCEEDED(hr));
            check("unmapped_resets",
                  get_app_data(mouse, btn0_id, &v0) && v0 == 0x101 &&
                  get_app_data(mouse, btn1_id, &v1) && v1 == 0 &&
                  get_app_data(mouse, btn2_id, &v2) && v2 == 0);
        }
    }

    /* 5. Priority-2 generic buttons (DIBUTTON_ANY) reused from keyboard to mouse. */
    {
        DIACTIONW actions[11];
        DIACTIONFORMATW af;
        UINT_PTR v0 = 0, v1 = 0, v2 = 0;

        memset(actions, 0, sizeof(actions));
        actions[0].uAppData = 0x101;
        actions[0].dwSemantic = DIMOUSE_BUTTON0;
        actions[0].lptszActionName = L"M0";
        actions[1].uAppData = 0x102;
        actions[1].dwSemantic = DIMOUSE_BUTTON1;
        actions[1].lptszActionName = L"M1";
        actions[2].uAppData = 0x103;
        actions[2].dwSemantic = DIMOUSE_BUTTON2;
        actions[2].lptszActionName = L"M2";
        for (int i = 0; i < 8; i++) {
            actions[3 + i].uAppData = 0x300 + i;
            actions[3 + i].dwSemantic = DIBUTTON_ANY(i + 1);
            actions[3 + i].lptszActionName = L"Any";
        }

        init_action_format(&af, actions, 11);
        hr = IDirectInputDevice8_BuildActionMap(kbd, &af, NULL, DIDBAM_DEFAULT);
        check("any_kbd_build", SUCCEEDED(hr));
        hr = IDirectInputDevice8_SetActionMap(kbd, &af, NULL, DIDSAM_DEFAULT);
        check("any_kbd_set", SUCCEEDED(hr));
        hr = IDirectInputDevice8_BuildActionMap(mouse, &af, NULL, DIDBAM_DEFAULT);
        check("any_mouse_build", SUCCEEDED(hr));
        hr = IDirectInputDevice8_SetActionMap(mouse, &af, NULL, DIDSAM_DEFAULT);
        check("any_mouse_set", SUCCEEDED(hr));
        check("generic_button_reuse",
              get_app_data(mouse, actions[0].dwObjID, &v0) && v0 == 0x101 &&
              get_app_data(mouse, actions[1].dwObjID, &v1) && v1 == 0x102 &&
              get_app_data(mouse, actions[2].dwObjID, &v2) && v2 == 0x103);
    }

    IDirectInputDevice8_Release(mouse);
    IDirectInputDevice8_Release(kbd);
    IDirectInput8_Release(dinput);
    return failures ? 1 : 0;
}
