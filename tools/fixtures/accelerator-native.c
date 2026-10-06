/* Authored Windows SDK accelerator oracle. No desktop input or visible windows. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <stddef.h>

void *memset(void *p, int value, unsigned size) {
    BYTE *out = (BYTE *)p;
    while (size--) *out++ = (BYTE)value;
    return p;
}
static DWORD sequence, command_count, command_message, command_wparam, command_lparam;
static DWORD menu_count, menu_events[8][3];
static void escape_json(char *out, DWORD capacity, const char *in) {
    static const char hex[] = "0123456789abcdef";
    DWORD at = 0;
    while (*in) {
        BYTE ch = (BYTE)*in++;
        if (at + 7 >= capacity) ExitProcess(99);
        if (ch == '"' || ch == '\\') { out[at++] = '\\'; out[at++] = ch; }
        else if (ch < 32) {
            out[at++] = '\\'; out[at++] = 'u'; out[at++] = '0'; out[at++] = '0';
            out[at++] = hex[ch >> 4]; out[at++] = hex[ch & 15];
        } else out[at++] = ch;
    }
    out[at] = 0;
}
static void output(const char *text, DWORD length) {
    DWORD written;
    if (!WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), text, length, &written, NULL) || written != length)
        ExitProcess(90);
}
static void row(const char *op, const char *label, int result, DWORD error, DWORD a, DWORD b, DWORD c) {
    char line[1024], escaped_op[128], escaped_label[256];
    escape_json(escaped_op, sizeof(escaped_op), op);
    escape_json(escaped_label, sizeof(escaped_label), label);
    int length = wsprintfA(line,
        "{\"seq\":%u,\"op\":\"%s\",\"label\":\"%s\",\"result\":%d,\"error\":%u,\"a\":%u,\"b\":%u,\"c\":%u}\n",
        ++sequence, escaped_op, escaped_label, result, error, a, b, c);
    output(line, length);
}
static void bytes(const char *label, const void *data, DWORD size) {
    const BYTE *in = (const BYTE *)data;
    static const char hex[] = "0123456789abcdef";
    char line[2048], escaped_label[256];
    escape_json(escaped_label, sizeof(escaped_label), label);
    DWORD at = wsprintfA(line, "{\"seq\":%u,\"op\":\"bytes\",\"label\":\"%s\",\"size\":%u,\"hex\":\"", ++sequence, escaped_label, size);
    if (size > 400) ExitProcess(91);
    for (DWORD i = 0; i < size; ++i) {
        line[at++] = hex[in[i] >> 4]; line[at++] = hex[in[i] & 15];
    }
    line[at++] = '"'; line[at++] = '}'; line[at++] = '\n';
    output(line, at);
}
static void copy_table(HACCEL table, const char *label, BOOL wide, int capacity, BOOL null_buffer) {
    ACCEL copy[16];
    memset(copy, 0xcc, sizeof(copy));
    SetLastError(0x1234);
    int count = wide ? CopyAcceleratorTableW(table, null_buffer ? NULL : copy, capacity)
                     : CopyAcceleratorTableA(table, null_buffer ? NULL : copy, capacity);
    DWORD error = GetLastError();
    row("copy", label, count, error, (DWORD)table, wide, capacity);
    if (!null_buffer) {
        for (int i = 0; i < count && i < 16; ++i)
            row("entry", label, i, 0, copy[i].fVirt, copy[i].key, copy[i].cmd);
        bytes(label, copy, sizeof(copy));
    }
}
static HACCEL create_table(ACCEL *entries, int count, BOOL wide, const char *label) {
    SetLastError(0x1234);
    HACCEL table = wide ? CreateAcceleratorTableW(entries, count) : CreateAcceleratorTableA(entries, count);
    DWORD error = GetLastError();
    row("create", label, table != NULL, error, (DWORD)table, wide, count);
    return table;
}
static void destroy_table(HACCEL table, const char *label) {
    SetLastError(0x1234);
    BOOL result = DestroyAcceleratorTable(table);
    DWORD error = GetLastError();
    row("destroy", label, result, error, (DWORD)table, 0, 0);
}
static LRESULT CALLBACK window_proc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    if ((message == WM_INITMENU || message == WM_INITMENUPOPUP) && menu_count < 8) {
        menu_events[menu_count][0] = message;
        menu_events[menu_count][1] = (DWORD)wparam;
        menu_events[menu_count++][2] = (DWORD)lparam;
        return 0;
    }
    if (message == WM_COMMAND || message == WM_SYSCOMMAND) {
        ++command_count;
        command_message = message;
        command_wparam = (DWORD)wparam;
        command_lparam = (DWORD)lparam;
        return 0;
    }
    return DefWindowProcA(hwnd, message, wparam, lparam);
}
static void translate_input(HWND hwnd, HACCEL table, const char *label, UINT message, WORD key, BYTE mods, DWORD input_lparam, BOOL wide, BOOL raw) {
    BYTE keys[256];
    MSG msg;
    memset(keys, 0, sizeof(keys));
    if (mods & FCONTROL) keys[VK_CONTROL] = keys[VK_LCONTROL] = 0x80;
    if (mods & FSHIFT) keys[VK_SHIFT] = keys[VK_LSHIFT] = 0x80;
    if (mods & FALT) keys[VK_MENU] = keys[VK_LMENU] = 0x80;
    /* Direct character probes separate Translate's conversion from Post/Peek.
       Other cases use only our owned window's authored queue message. */
    if (raw) {
        memset(&msg, 0, sizeof(msg));
        msg.hwnd = hwnd; msg.message = message; msg.wParam = key; msg.lParam = input_lparam;
    } else if (wide) {
        if (!PostMessageW(hwnd, message, key, input_lparam) ||
            !PeekMessageW(&msg, hwnd, message, message, PM_REMOVE)) ExitProcess(92);
    } else if (!PostMessageA(hwnd, message, key, input_lparam) ||
        !PeekMessageA(&msg, hwnd, message, message, PM_REMOVE)) ExitProcess(92);
    if (!SetKeyboardState(keys)) ExitProcess(92);
    row("message_input", label, raw, 0, msg.message, (DWORD)msg.wParam, (DWORD)msg.lParam);
    row("key_state", label, 1, 0, (WORD)GetKeyState(VK_CONTROL), (WORD)GetKeyState(VK_SHIFT), (WORD)GetKeyState(VK_MENU));
    command_count = command_message = command_wparam = command_lparam = 0;
    menu_count = 0;
    SetLastError(0x1234);
    int result = wide ? TranslateAcceleratorW(hwnd, table, &msg) : TranslateAcceleratorA(hwnd, table, &msg);
    DWORD error = GetLastError();
    row("translate", label, result, error, message, key, mods);
    row("command", label, command_count, 0, command_message, command_wparam, command_lparam);
    for (DWORD i = 0; i < menu_count; ++i)
        row("menu_event", label, i, 0, menu_events[i][0], menu_events[i][1], menu_events[i][2]);
    row("message_after", label, 0, 0, msg.message, (DWORD)msg.wParam, (DWORD)msg.lParam);
}
static void translate(HWND hwnd, HACCEL table, const char *label, UINT message, WORD key, BYTE mods, DWORD input_lparam, BOOL wide) {
    translate_input(hwnd, table, label, message, key, mods, input_lparam, wide, FALSE);
}
static void translate_raw(HWND hwnd, HACCEL table, const char *label, UINT message, WORD key, BYTE mods, DWORD input_lparam, BOOL wide) {
    translate_input(hwnd, table, label, message, key, mods, input_lparam, wide, TRUE);
}
static void resource_bytes(HMODULE module, LPCSTR name, const char *label) {
    SetLastError(0x1234);
    HRSRC resource = FindResourceA(module, name, RT_ACCELERATOR);
    row("find_resource", label, resource != NULL, GetLastError(), (DWORD)resource, 0, 0);
    if (!resource) return;
    HGLOBAL loaded = LoadResource(module, resource);
    const void *data = LockResource(loaded);
    if (!data) ExitProcess(94);
    bytes(label, data, SizeofResource(module, resource));
}
static BOOL CALLBACK resource_name(HMODULE module, LPCSTR type, LPSTR name, LONG_PTR data) {
    (void)data;
    HRSRC resource = FindResourceA(module, name, type);
    row("enum_resource", IS_INTRESOURCE(name) ? "ordinal" : name, resource != NULL, GetLastError(), (DWORD)name, (DWORD)resource, 0);
    return TRUE;
}
void mainCRTStartup(void) {
    BYTE saved_keys[256];
    HINSTANCE module = GetModuleHandleA(NULL);
    if (!GetKeyboardState(saved_keys)) ExitProcess(89);
    row("json_escape", "quote\" slash\\ newline\n", 1, 0, 0, 0, 0);
    row("layout", "ACCEL", sizeof(ACCEL), 0, offsetof(ACCEL, fVirt), offsetof(ACCEL, key), offsetof(ACCEL, cmd));
    row("environment", "codepages", GetACP(), 0, GetOEMCP(), sizeof(void *), 0);
    WNDCLASSA klass;
    memset(&klass, 0, sizeof(klass));
    klass.lpfnWndProc = window_proc; klass.hInstance = module; klass.lpszClassName = "OwnedAcceleratorOracle";
    if (!RegisterClassA(&klass)) ExitProcess(95);
    HWND hwnd = CreateWindowExA(WS_EX_NOACTIVATE, klass.lpszClassName, "owned-hidden-oracle", 0,
        0, 0, 0, 0, HWND_MESSAGE, NULL, module, NULL);
    if (!hwnd) ExitProcess(96);
    ACCEL entries[] = {
        { FVIRTKEY | FCONTROL, 'Q', 1101 },
        { FVIRTKEY | FSHIFT | FNOINVERT, 'S', 1102 },
        { FVIRTKEY | FALT, 'M', 1103 },
        { FVIRTKEY | FCONTROL | FSHIFT, 'F', 1104 },
        { 0, 'x', 1105 }
    };
    HACCEL created = create_table(entries, 5, FALSE, "ordinary_A");
    HACCEL created2 = create_table(entries, 5, TRUE, "ordinary_W");
    if (!created || !created2) ExitProcess(97);
    row("identity", "created_same_content", created == created2, 0, (DWORD)created, (DWORD)created2, 0);
    bytes("create_input", entries, sizeof(entries));
    entries[0].cmd = 9999;
    copy_table(created, "created_query_zero", FALSE, 0, TRUE);
    copy_table(created, "created_query_negative", TRUE, -1, TRUE);
    copy_table(created, "created_zero", FALSE, 0, FALSE);
    copy_table(created, "created_prefix", FALSE, 1, FALSE);
    copy_table(created, "created_full_A", FALSE, 16, FALSE);
    copy_table(created, "created_full_W", TRUE, 16, FALSE);
    create_table(entries, 0, FALSE, "zero_count");
    create_table(NULL, 0, FALSE, "null_zero_count");
    create_table(NULL, 1, TRUE, "null_nonzero_count");
    ACCEL unusual[] = { { 0xff, 'Z', 2101 }, { 0x80, 'z', 2102 }, { FVIRTKEY, 'N', 2103 } };
    HACCEL flags = create_table(unusual, 3, TRUE, "reserved_flags");
    copy_table(flags, "reserved_flags_W", TRUE, 16, FALSE);
    destroy_table(flags, "reserved_flags");
    ACCEL encoded[] = { { 0, 0xa082, 2201 }, { 0, 0x82a0, 2202 }, { 0, 0xa2a4, 2204 }, { 0, 0x0141, 2205 }, { 0, 0xa4a2, 2206 } };
    HACCEL ansi = create_table(encoded, 5, FALSE, "packed_ANSI_words");
    copy_table(ansi, "packed_A_to_A", FALSE, 16, FALSE);
    copy_table(ansi, "packed_A_to_W", TRUE, 16, FALSE);
    ACCEL unicode[] = { { 0, 0x3042, 2203 }, { FALT, 0x3042, 2207 } };
    HACCEL wide = create_table(unicode, 2, TRUE, "Unicode_hiragana");
    copy_table(wide, "hiragana_W_to_A", FALSE, 16, FALSE);
    copy_table(wide, "hiragana_W_to_W", TRUE, 16, FALSE);
    ACCEL roundtrip_entry;
    CopyAcceleratorTableA(wide, &roundtrip_entry, 1);
    HACCEL roundtrip = create_table(&roundtrip_entry, 1, FALSE, "W_copy_A_create_A_roundtrip");
    copy_table(roundtrip, "W_copy_A_create_A_to_W", TRUE, 16, FALSE);
    destroy_table(roundtrip, "W_copy_A_create_A_roundtrip");
    translate_raw(hwnd, wide, "Unicode_char_A_A2A4", WM_CHAR, 0xa2a4, 0, 1, FALSE);
    translate_raw(hwnd, wide, "Unicode_char_A_A4A2", WM_CHAR, 0xa4a2, 0, 1, FALSE);
    translate_raw(hwnd, wide, "Unicode_char_A_3042", WM_CHAR, 0x3042, 0, 1, FALSE);
    translate_raw(hwnd, wide, "Unicode_char_W_3042", WM_CHAR, 0x3042, 0, 1, TRUE);
    translate_raw(hwnd, wide, "Unicode_syschar_A_A2A4", WM_SYSCHAR, 0xa2a4, FALT, 0x20000001, FALSE);
    translate_raw(hwnd, wide, "Unicode_syschar_A_A4A2", WM_SYSCHAR, 0xa4a2, FALT, 0x20000001, FALSE);
    translate_raw(hwnd, wide, "Unicode_syschar_W_3042", WM_SYSCHAR, 0x3042, FALT, 0x20000001, TRUE);
    destroy_table(ansi, "packed_ANSI_words"); destroy_table(wide, "Unicode_hiragana");
    EnumResourceNamesA(module, RT_ACCELERATOR, resource_name, 0);
    resource_bytes(module, MAKEINTRESOURCEA(101), "resource_id_raw");
    resource_bytes(module, "NATIVENAMED", "resource_name_raw");
    SetLastError(0x1234);
    HACCEL loaded = LoadAcceleratorsA(module, MAKEINTRESOURCEA(101));
    row("load", "resource_id_A", loaded != NULL, GetLastError(), (DWORD)loaded, 0, 0);
    HACCEL loaded2 = LoadAcceleratorsA(module, MAKEINTRESOURCEA(101));
    HACCEL loadedw = LoadAcceleratorsW(module, MAKEINTRESOURCEW(101));
    HACCEL named = LoadAcceleratorsA(module, "NATIVENAMED");
    HACCEL namedw = LoadAcceleratorsW(module, L"NATIVENAMED");
    row("identity", "same_resource_A_A_W", loaded == loaded2 && loaded == loadedw, 0, (DWORD)loaded, (DWORD)loaded2, (DWORD)loadedw);
    row("identity", "same_named_A_W", named && named == namedw, 0, (DWORD)named, (DWORD)namedw, 0);
    row("identity", "different_resources_same_bytes", loaded == named, 0, (DWORD)loaded, (DWORD)named, 0);
    copy_table(loaded, "loaded_full_A", FALSE, 16, FALSE);
    copy_table(loadedw, "loaded_full_W", TRUE, 16, FALSE);
    SetLastError(0x1234);
    HACCEL missing = LoadAcceleratorsA(module, "AbsentAuthoredResource");
    row("load", "missing_resource", missing != NULL, GetLastError(), (DWORD)missing, 0, 0);
    SetLastError(0x1234);
    HACCEL mixed_case = LoadAcceleratorsA(module, "NativeNamed");
    row("load", "mixed_case_resource_name", mixed_case != NULL, GetLastError(), (DWORD)mixed_case, 0, 0);
    translate(hwnd, created, "ctrl_Q", WM_KEYDOWN, 'Q', FCONTROL, 1, FALSE);
    translate(hwnd, created, "plain_Q", WM_KEYDOWN, 'Q', 0, 1, FALSE);
    translate(hwnd, created, "extra_shift_Q", WM_KEYDOWN, 'Q', FCONTROL | FSHIFT, 1, FALSE);
    translate(hwnd, created, "extra_alt_Q", WM_SYSKEYDOWN, 'Q', FCONTROL | FALT, 0x20000001, FALSE);
    translate(hwnd, created, "shift_S", WM_KEYDOWN, 'S', FSHIFT, 1, FALSE);
    translate(hwnd, created, "plain_S", WM_KEYDOWN, 'S', 0, 1, FALSE);
    translate(hwnd, created, "alt_M_sys", WM_SYSKEYDOWN, 'M', FALT, 0x20000001, FALSE);
    translate(hwnd, created, "alt_M_key", WM_KEYDOWN, 'M', FALT, 1, FALSE);
    translate(hwnd, created, "no_alt_M_context", WM_SYSKEYDOWN, 'M', 0, 0x20000001, FALSE);
    translate(hwnd, created, "ctrl_shift_F", WM_KEYDOWN, 'F', FCONTROL | FSHIFT, 1, TRUE);
    translate(hwnd, loaded, "loaded_ctrl_Q", WM_KEYDOWN, 'Q', FCONTROL, 1, FALSE);
    translate(hwnd, created, "ctrl_Q_keyup", WM_KEYUP, 'Q', FCONTROL, 0xc0000001, FALSE);
    translate(hwnd, created, "char_x", WM_CHAR, 'x', 0, 1, FALSE);
    translate(hwnd, created, "char_x_shift", WM_CHAR, 'x', FSHIFT, 1, FALSE);
    translate(hwnd, created, "char_x_ctrl", WM_CHAR, 'x', FCONTROL, 1, FALSE);
    translate(hwnd, created, "char_x_alt", WM_CHAR, 'x', FALT, 0x20000001, FALSE);
    translate(hwnd, created, "syschar_x", WM_SYSCHAR, 'x', FALT, 0x20000001, FALSE);
    translate(hwnd, created, "key_x", WM_KEYDOWN, 'x', 0, 1, FALSE);
    ACCEL characters[] = { { FSHIFT, 's', 3101 }, { FCONTROL, 'c', 3102 }, { FALT, 'a', 3103 } };
    HACCEL chars = create_table(characters, 3, FALSE, "character_modifiers");
    translate(hwnd, chars, "char_s_plain", WM_CHAR, 's', 0, 1, FALSE);
    translate(hwnd, chars, "char_s_shift", WM_CHAR, 's', FSHIFT, 1, FALSE);
    translate(hwnd, chars, "char_c_plain", WM_CHAR, 'c', 0, 1, FALSE);
    translate(hwnd, chars, "char_c_ctrl", WM_CHAR, 'c', FCONTROL, 1, FALSE);
    translate(hwnd, chars, "char_a_alt", WM_CHAR, 'a', FALT, 0x20000001, FALSE);
    translate(hwnd, chars, "syschar_a_alt", WM_SYSCHAR, 'a', FALT, 0x20000001, FALSE);
    translate(hwnd, chars, "syschar_a_no_alt", WM_SYSCHAR, 'a', 0, 1, FALSE);
    translate(hwnd, created, "char_x_noalt_context", WM_CHAR, 'x', 0, 0x20000001, FALSE);
    translate(hwnd, created, "char_x_alt_nocontext", WM_CHAR, 'x', FALT, 1, FALSE);
    translate(hwnd, chars, "char_a_noalt_context", WM_CHAR, 'a', 0, 0x20000001, FALSE);
    translate(hwnd, chars, "char_a_alt_nocontext", WM_CHAR, 'a', FALT, 1, FALSE);
    translate(hwnd, chars, "syschar_a_noalt_context", WM_SYSCHAR, 'a', 0, 0x20000001, FALSE);
    translate(hwnd, chars, "syschar_a_alt_nocontext", WM_SYSCHAR, 'a', FALT, 1, FALSE);
    ACCEL duplicates[] = { { FVIRTKEY | FCONTROL, 'Q', 3201 }, { FVIRTKEY | FCONTROL, 'Q', 3202 } };
    HACCEL duplicate = create_table(duplicates, 2, TRUE, "duplicate_keys");
    translate(hwnd, duplicate, "duplicate_first", WM_KEYDOWN, 'Q', FCONTROL, 1, TRUE);
    destroy_table(duplicate, "duplicate_keys"); destroy_table(chars, "character_modifiers");
    EnableWindow(hwnd, FALSE);
    row("window", "disabled_no_menu_state", IsWindowEnabled(hwnd), 0, IsWindowVisible(hwnd), IsIconic(hwnd), 0);
    translate(hwnd, created, "disabled_no_menu", WM_KEYDOWN, 'Q', FCONTROL, 1, FALSE);
    EnableWindow(hwnd, TRUE);
    /* Never show, activate, minimize or capture with this owned menu window. */
    HWND menu_window = CreateWindowExA(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, klass.lpszClassName,
        "owned-hidden-menu-oracle", WS_OVERLAPPEDWINDOW, -32000, -32000, 64, 64, NULL, NULL, module, NULL);
    HMENU menu = CreateMenu(), popup = CreatePopupMenu();
    if (!menu_window || !menu || !popup || !AppendMenuA(popup, MF_STRING, 1101, "OwnedCommand") ||
        !AppendMenuA(menu, MF_POPUP, (UINT_PTR)popup, "OwnedPopup") || !SetMenu(menu_window, menu)) ExitProcess(98);
    row("window", "hidden_menu", IsWindowVisible(menu_window), 0, IsIconic(menu_window), (DWORD)menu, (DWORD)popup);
    translate(menu_window, created, "enabled_menu_command", WM_KEYDOWN, 'Q', FCONTROL, 1, FALSE);
    EnableMenuItem(menu, 1101, MF_BYCOMMAND | MF_GRAYED);
    translate(menu_window, created, "disabled_menu_command", WM_KEYDOWN, 'Q', FCONTROL, 1, FALSE);
    EnableMenuItem(menu, 1101, MF_BYCOMMAND | MF_ENABLED);
    EnableWindow(menu_window, FALSE);
    row("window", "disabled_window_menu_state", IsWindowEnabled(menu_window), 0, IsWindowVisible(menu_window), IsIconic(menu_window), 0);
    translate(menu_window, created, "disabled_window_menu", WM_KEYDOWN, 'Q', FCONTROL, 1, FALSE);
    EnableWindow(menu_window, TRUE);
    ACCEL system_entry[] = { { FVIRTKEY, VK_F9, SC_CLOSE } };
    HACCEL system_table = create_table(system_entry, 1, TRUE, "system_close");
    translate(menu_window, system_table, "system_close_command", WM_KEYDOWN, VK_F9, 0, 1, TRUE);
    destroy_table(system_table, "system_close");
    SetMenu(menu_window, NULL); DestroyMenu(menu); DestroyWindow(menu_window);
    destroy_table(created, "created_first");
    copy_table(created, "created_after_destroy", FALSE, 0, TRUE);
    destroy_table(created, "created_twice");
    destroy_table(created2, "created_W");
    destroy_table(loaded, "loaded_first_destroy");
    copy_table(loaded2, "loaded_after_destroy", FALSE, 0, TRUE);
    SetLastError(0x1234);
    HACCEL reloaded = LoadAcceleratorsA(module, MAKEINTRESOURCEA(101));
    row("identity", "reloaded_after_destroy", reloaded == loaded, GetLastError(), (DWORD)loaded, (DWORD)reloaded, 0);
    destroy_table(loaded2, "loaded_second_destroy"); destroy_table(loadedw, "loaded_third_destroy");
    copy_table(reloaded, "loaded_after_three_destroys", FALSE, 0, TRUE);
    destroy_table(reloaded, "reloaded_destroy");
    copy_table(reloaded, "loaded_after_final_destroy", FALSE, 0, TRUE);
    SetLastError(0x1234);
    HACCEL new_loaded = LoadAcceleratorsA(module, MAKEINTRESOURCEA(101));
    row("identity", "load_after_final_destroy", new_loaded == loaded, GetLastError(), (DWORD)loaded, (DWORD)new_loaded, 0);
    destroy_table(new_loaded, "new_loaded_destroy");
    destroy_table(named, "named_first_destroy");
    copy_table(namedw, "named_after_destroy", TRUE, 0, TRUE);
    destroy_table(namedw, "named_second_destroy");
    copy_table(namedw, "named_after_two_destroys", TRUE, 0, TRUE);
    destroy_table(mixed_case, "named_third_destroy");
    copy_table(namedw, "named_after_final_destroy", TRUE, 0, TRUE);
    copy_table((HACCEL)0x1234, "invalid_handle", FALSE, 0, TRUE);
    destroy_table((HACCEL)0x1234, "invalid_handle");
    SetKeyboardState(saved_keys);
    DestroyWindow(hwnd); UnregisterClassA(klass.lpszClassName, module);
    row("complete", "owned_native_oracle", 1, 0, 0, 0, 0);
    ExitProcess(0);
}
