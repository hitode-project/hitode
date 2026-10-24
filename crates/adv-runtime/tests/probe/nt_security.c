/* Build: i686-w64-mingw32-gcc -O2 -Wall nt_security.c -o nt_security.exe -ladvapi32 -lntdll
 * Run headless. Authored security objects and tokens only.
 */
#include <windows.h>
#include <winternl.h>
#include <stdio.h>

#define DOS_DRIVE_ROOT "Z:\\" /* privacy: allow -- authored guest drive mapping */

/* These NT services have no Win32 wrapper. */
NTSYSAPI NTSTATUS NTAPI NtCreateToken(PHANDLE, ACCESS_MASK, POBJECT_ATTRIBUTES,
    TOKEN_TYPE, PLUID, PLARGE_INTEGER, PTOKEN_USER, PTOKEN_GROUPS,
    PTOKEN_PRIVILEGES, PTOKEN_OWNER, PTOKEN_PRIMARY_GROUP, PTOKEN_DEFAULT_DACL, PTOKEN_SOURCE);
NTSYSAPI NTSTATUS NTAPI NtAllocateReserveObject(PHANDLE, POBJECT_ATTRIBUTES, ULONG);
NTSYSAPI NTSTATUS NTAPI NtMakePermanentObject(HANDLE);
NTSYSAPI NTSTATUS NTAPI NtMakeTemporaryObject(HANDLE);
NTSYSAPI NTSTATUS NTAPI NtOpenProcessTokenEx(HANDLE, ACCESS_MASK, ULONG, PHANDLE);
NTSYSAPI NTSTATUS NTAPI NtOpenThreadTokenEx(HANDLE, ACCESS_MASK, BOOLEAN, ULONG, PHANDLE);

static void check(const char *name, BOOL result)
{
    printf("%s %s\n", name, result ? "ok" : "FAIL");
    if (!result) { printf("error %lu\n", GetLastError()); ExitProcess(1); }
}

int main(void)
{
    HANDLE token, imp, thread_token, extended, filtered, created, event, other, copy, reserve;
    BYTE user_buffer[128], created_user[128], acl_buffer[64], sd_buffer[1024], dacl_buffer[128];
    TOKEN_USER *user = (TOKEN_USER *)user_buffer;
    TOKEN_GROUPS groups;
    TOKEN_PRIVILEGES privileges = {0}, previous;
    TOKEN_OWNER owner;
    TOKEN_PRIMARY_GROUP primary;
    TOKEN_DEFAULT_DACL dacl;
    TOKEN_SOURCE source = {{'p','r','o','b','e',0,0,0}, {0,0}};
    SECURITY_DESCRIPTOR sd;
    OBJECT_ATTRIBUTES attr = {sizeof(attr)};
    LARGE_INTEGER expiration;
    LUID id;
    PRIVILEGE_SET required = {0}, used;
    GENERIC_MAPPING mapping = {1, 2, 4, 7};
    DWORD size, granted;
    DWORD drives, written, serial;
    HANDLE file;
    char contents[4];
    BOOL present, allowed, defaulted;
    PACL read_acl;
    PSID world;
    SID_IDENTIFIER_AUTHORITY world_authority = SECURITY_WORLD_SID_AUTHORITY;
    BOOL (WINAPI *compare)(HANDLE, HANDLE) = (void *)GetProcAddress(GetModuleHandleA("kernelbase.dll"), "CompareObjectHandles");

    check("open_process", OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &token));
    check("open_process_ex", !NtOpenProcessTokenEx(GetCurrentProcess(), TOKEN_QUERY, 2, &extended)
          && GetHandleInformation(extended, &size) && (size & HANDLE_FLAG_INHERIT));
    CloseHandle(extended);
    check("fixed_user", GetTokenInformation(token, TokenUser, user_buffer, sizeof(user_buffer), &size)
          && *GetSidSubAuthorityCount(user->User.Sid) == 5
          && GetSidIdentifierAuthority(user->User.Sid)->Value[5] == 5
          && *GetSidSubAuthority(user->User.Sid, 0) == 21
          && *GetSidSubAuthority(user->User.Sid, 1) == 0
          && *GetSidSubAuthority(user->User.Sid, 2) == 0
          && *GetSidSubAuthority(user->User.Sid, 3) == 0
          && *GetSidSubAuthority(user->User.Sid, 4) == 1000);
    check("duplicate_token", DuplicateTokenEx(token, TOKEN_ALL_ACCESS, NULL, SecurityImpersonation, TokenImpersonation, &imp));
    check("open_thread_empty", !OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &thread_token) && GetLastError() == ERROR_NO_TOKEN);
    check("set_thread_token", SetThreadToken(NULL, imp));
    check("open_thread", OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &thread_token));
    check("open_thread_ex", !NtOpenThreadTokenEx(GetCurrentThread(), TOKEN_QUERY, TRUE, 2, &extended)
          && GetHandleInformation(extended, &size) && (size & HANDLE_FLAG_INHERIT));
    CloseHandle(extended);
    CloseHandle(thread_token);
    check("revert", RevertToSelf());

    privileges.PrivilegeCount = 1;
    check("lookup_privilege", LookupPrivilegeValueA(NULL, SE_CHANGE_NOTIFY_NAME, &privileges.Privileges[0].Luid));
    privileges.Privileges[0].Attributes = 0;
    check("adjust_privileges", AdjustTokenPrivileges(imp, FALSE, &privileges, sizeof(previous), &previous, &size)
          && GetLastError() == ERROR_SUCCESS && previous.PrivilegeCount == 1 && size == sizeof(previous));
    required.PrivilegeCount = 1;
    required.Control = PRIVILEGE_SET_ALL_NECESSARY;
    required.Privilege[0].Luid = privileges.Privileges[0].Luid;
    check("privilege_disabled", PrivilegeCheck(imp, &required, &present) && !present);
    check("restore_privileges", AdjustTokenPrivileges(imp, FALSE, &previous, 0, NULL, NULL) && GetLastError() == ERROR_SUCCESS);
    check("privilege_enabled", PrivilegeCheck(imp, &required, &present) && present
          && (required.Privilege[0].Attributes & SE_PRIVILEGE_USED_FOR_ACCESS));
    check("filter_token", CreateRestrictedToken(token, 0, 0, NULL, 1, &privileges.Privileges[0], 0, NULL, &filtered));
    check("filtered_privilege", PrivilegeCheck(filtered, &required, &present) && !present);
    CloseHandle(filtered);

    check("default_dacl_init", InitializeAcl((PACL)acl_buffer, sizeof(ACL), ACL_REVISION));
    dacl.DefaultDacl = (PACL)acl_buffer;
    check("set_token_dacl", SetTokenInformation(imp, TokenDefaultDacl, &dacl, sizeof(dacl)));
    check("token_dacl_roundtrip", GetTokenInformation(imp, TokenDefaultDacl, dacl_buffer, sizeof(dacl_buffer), &size)
          && ((TOKEN_DEFAULT_DACL *)dacl_buffer)->DefaultDacl->AceCount == 0
          && ((TOKEN_DEFAULT_DACL *)dacl_buffer)->DefaultDacl->AclSize == sizeof(ACL));
    check("set_token_short", !SetTokenInformation(imp, TokenDefaultDacl, &dacl, 0) && GetLastError() == ERROR_BAD_LENGTH);

    check("world_sid", AllocateAndInitializeSid(&world_authority, 1, SECURITY_WORLD_RID, 0,0,0,0,0,0,0, &world));
    check("security_init", InitializeSecurityDescriptor(&sd, SECURITY_DESCRIPTOR_REVISION)
          && SetSecurityDescriptorOwner(&sd, user->User.Sid, FALSE)
          && SetSecurityDescriptorGroup(&sd, world, FALSE)
          && InitializeAcl((PACL)acl_buffer, sizeof(acl_buffer), ACL_REVISION)
          && AddAccessAllowedAce((PACL)acl_buffer, ACL_REVISION, 1, world)
          && SetSecurityDescriptorDacl(&sd, TRUE, (PACL)acl_buffer, FALSE));
    size = sizeof(used);
    check("access_allowed", AccessCheck(&sd, imp, 1, &mapping, &used, &size, &granted, &allowed) && allowed && granted == 1);
    size = sizeof(used);
    check("access_denied", AccessCheck(&sd, imp, 2, &mapping, &used, &size, &granted, &allowed) && !allowed && granted == 0);
    size = 0;
    check("access_short", !AccessCheck(&sd, imp, 1, &mapping, &used, &size, &granted, &allowed)
          && GetLastError() == ERROR_INSUFFICIENT_BUFFER && size == sizeof(PRIVILEGE_SET));
    event = CreateEventA(NULL, FALSE, FALSE, "nt-security-object");
    check("create_event", event != NULL);
    check("set_security", InitializeAcl((PACL)acl_buffer, sizeof(acl_buffer), ACL_REVISION)
          && AddAccessAllowedAce((PACL)acl_buffer, ACL_REVISION, EVENT_ALL_ACCESS, world)
          && SetKernelObjectSecurity(event, DACL_SECURITY_INFORMATION, &sd));
    size = 0;
    check("query_security_short", !GetKernelObjectSecurity(event, DACL_SECURITY_INFORMATION, NULL, 0, &size)
          && GetLastError() == ERROR_INSUFFICIENT_BUFFER && size >= sizeof(SECURITY_DESCRIPTOR));
    check("query_security", GetKernelObjectSecurity(event, DACL_SECURITY_INFORMATION, sd_buffer, sizeof(sd_buffer), &size)
          && IsValidSecurityDescriptor(sd_buffer) && GetSecurityDescriptorDacl(sd_buffer, &present, &read_acl, &defaulted)
          && present && read_acl && read_acl->AceCount == 1);
    check("duplicate_handle", DuplicateHandle(GetCurrentProcess(), event, GetCurrentProcess(), &copy, 0, FALSE, DUPLICATE_SAME_ACCESS));
    other = CreateEventA(NULL, FALSE, FALSE, NULL);
    check("compare_objects", compare && compare(event, copy) && !compare(event, other));
    CloseHandle(copy);
    CloseHandle(other);
    check("make_permanent", !NtMakePermanentObject(event));
    CloseHandle(event);
    event = OpenEventA(EVENT_ALL_ACCESS, FALSE, "nt-security-object");
    check("permanent_lifetime", event != NULL);
    check("make_temporary", !NtMakeTemporaryObject(event));
    CloseHandle(event);
    check("temporary_lifetime", !OpenEventA(EVENT_ALL_ACCESS, FALSE, "nt-security-object") && GetLastError() == ERROR_FILE_NOT_FOUND);
    drives = GetLogicalDrives();
    check("logical_drives", (drives & 0xc) == 0xc && !(drives & (1u << 25)));
    check("dos_device_directory", CreateDirectoryA("C:\\nt-drive-test", NULL));
    check("dos_device_create", DefineDosDeviceA(DDD_RAW_TARGET_PATH, "Z:", "\\??\\C:\\nt-drive-test"));
    check("dos_device_list", GetLogicalDrives() == (drives | (1u << 25)));
    file = CreateFileA(DOS_DRIVE_ROOT "file.bin", GENERIC_WRITE, 0, NULL, CREATE_NEW, 0, NULL);
    check("dos_device_write", file != INVALID_HANDLE_VALUE
          && WriteFile(file, "test", 4, &written, NULL) && written == 4);
    CloseHandle(file);
    check("dos_device_attributes", GetFileAttributesA(DOS_DRIVE_ROOT "file.bin") != INVALID_FILE_ATTRIBUTES);
    check("dos_device_missing_path", GetFileAttributesA(DOS_DRIVE_ROOT "missing\\file.bin") == INVALID_FILE_ATTRIBUTES
          && GetLastError() == ERROR_PATH_NOT_FOUND);
    file = CreateFileA("C:\\nt-drive-test\\file.bin", GENERIC_READ, 0, NULL, OPEN_EXISTING, 0, NULL);
    check("dos_device_read", file != INVALID_HANDLE_VALUE
          && ReadFile(file, contents, 4, &written, NULL) && written == 4 && !memcmp(contents, "test", 4));
    CloseHandle(file);
    check("dos_device_volume", GetVolumeInformationA(DOS_DRIVE_ROOT, NULL, 0, &serial, NULL, NULL, NULL, 0));
    check("dos_device_rename", MoveFileA(DOS_DRIVE_ROOT "file.bin", DOS_DRIVE_ROOT "renamed.bin"));
    check("dos_device_delete", DeleteFileA(DOS_DRIVE_ROOT "renamed.bin"));
    check("dos_device_remove", DefineDosDeviceA(DDD_REMOVE_DEFINITION, "Z:", NULL));
    check("dos_device_removed", GetLogicalDrives() == drives
          && GetFileAttributesA(DOS_DRIVE_ROOT) == INVALID_FILE_ATTRIBUTES);
    check("dos_device_cleanup", RemoveDirectoryA("C:\\nt-drive-test"));

    groups.GroupCount = 1;
    groups.Groups[0].Sid = world;
    groups.Groups[0].Attributes = SE_GROUP_ENABLED | SE_GROUP_ENABLED_BY_DEFAULT | SE_GROUP_MANDATORY;
    primary.PrimaryGroup = world;
    owner.Owner = user->User.Sid;
    privileges.PrivilegeCount = 0;
    expiration.QuadPart = 0x7fffffffffffffffLL;
    check("token_id", AllocateLocallyUniqueId(&id));
    check("create_token", !NtCreateToken(&created, TOKEN_ALL_ACCESS, &attr, TokenPrimary, &id, &expiration,
          user, &groups, &privileges, &owner, &primary, &dacl, &source));
    check("created_user", GetTokenInformation(created, TokenUser, created_user, sizeof(created_user), &size)
          && EqualSid(((TOKEN_USER *)created_user)->User.Sid, user->User.Sid));
    CloseHandle(created);
    primary.PrimaryGroup = user->User.Sid;
    created = (HANDLE)0xdeadbeef;
    check("invalid_primary_group", NtCreateToken(&created, TOKEN_ALL_ACCESS, &attr, TokenPrimary, &id, &expiration,
          user, &groups, &privileges, &owner, &primary, &dacl, &source) == (NTSTATUS)0xc000005b && !created);
    check("reserve_object", !NtAllocateReserveObject(&reserve, NULL, 1) && reserve != NULL);
    CloseHandle(reserve);
    check("reserve_invalid", NtAllocateReserveObject(&reserve, NULL, 2) == (NTSTATUS)0xc000000d && !reserve);
    FreeSid(world);
    CloseHandle(imp);
    CloseHandle(token);
    return 0;
}
