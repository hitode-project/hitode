# System libraries

ID prefix: SYS

This spec covers Windows system libraries outside the core subsystems, as an offline Windows 7 machine without printers or a console presents them. It includes networking, cryptography, process status, consoles, installers, profile files and printing. `adv-win32` implements them. [personality.md](personality.md) covers the shared Windows identity and [win32.md](win32.md) the core subsystems.

## Offline networking

### SYS-NET-1 Offline Internet sessions

**Rule**: Synchronous WinINet and WinHTTP initialization MUST create library-owned sessions independently of transport availability. Session queries and closure MUST validate library ownership. Offline URL retrieval MUST fail without creating requests or reporting capability gaps.
**Verification**: Test

### SYS-NET-2 Local network addresses

**Rule**: Winsock byte-order and numeric address conversions MUST operate without network I/O or requiring WSAStartup. Invalid text MUST preserve output, and failed pointer or family validation MUST set the documented Winsock error. Legacy Winsock aliases and socket closure MUST share initialization state, distinguishing uninitialized libraries from invalid sockets.
**Verification**: Test

### SYS-NET-3 Offline provider routing

**Rule**: MPR connection and external-resource requests MUST fail ordinarily in the offline profile without publishing redirects. Connected and remembered-resource enumerators MUST be real Objects aliases over the profile's empty resource lists. Provider names and current-user queries MUST use local configuration independently of transport availability.
**Verification**: Test

### SYS-NET-4 Provider extended errors

**Rule**: WNetSetLastError and WNetGetLastError MUST share extended error codes and UTF-16 strings per guest thread. A/W conversion MUST use the existing process codec. Extended-error queries MUST truncate strings with terminators without replacing another thread's information.
**Verification**: Test
**Basis**: Microsoft WNetSetLastError, WNetGetLastError and WNetGetUser documentation.

### SYS-NET-5 Provider capability refusal

**Rule**: Network-provider capability/version queries MUST report a capability gap when provider DLL dispatch is unavailable. They MUST NOT fabricate successful provider structures.
**Verification**: Test

### SYS-NET-6 Offline request handles

**Rule**: Request operations MUST reject session and stale handles using their documented failure conventions. Read and write failures MUST preserve or clear byte counts according to their validated handle and operation. Session timeout and user-agent changes MUST remain observable through existing A/W option queries.
**Verification**: Test
**Basis**: Microsoft WinINet handle types, InternetReadFile, InternetWriteFile and InternetSetOption contracts.

## Filesystem identity

### SYS-FILE-1 Invariant filesystem names

**Rule**: Filesystem name matching MUST use the shared invariant UTF-16 case mapping on every host. Non-ASCII names MUST use the same directory index and invalidation rules as ASCII names.
**Verification**: Test

### SYS-FILE-2 Delete on close

**Rule**: A file opened with FILE_FLAG_DELETE_ON_CLOSE MUST be deleted once no handle has it open, or when the process exits. Its handle MUST carry DELETE access, so other opens MUST share delete access.
**Verification**: Test
**Basis**: Microsoft CreateFile documentation of FILE_FLAG_DELETE_ON_CLOSE.

## Cryptography

### SYS-CRYPTO-1 Streaming digests

**Rule**: CryptoAPI MD5, SHA-1, SHA-256, SHA-384 and SHA-512 MUST compute their standard digests across arbitrary input splits. Hash duplication MUST copy an independently writable stream. Successful hash-value retrieval MUST finalize that hash, while size queries and short buffers MUST preserve its stream.
**Verification**: Test
**Basis**: RFC 1321, FIPS 180-4, and Microsoft CryptGetHashParam documentation.

### SYS-CRYPTO-2 CSP ownership

**Rule**: CryptoAPI contexts and hashes MUST use typed aliases in the shared Objects owner. Context reference counts MUST control context lifetime. Invalid handles and failed input validation MUST preserve hash state and unrelated output bytes.
**Verification**: Test

### SYS-CRYPTO-3 Symmetric transforms

**Rule**: CryptoAPI AES-128/192/256, DES, triple DES, RC2 and RC4 MUST perform their specified symmetric transforms. Key duplication MUST preserve an independent cipher state. Size queries and rejected calls MUST NOT advance cipher state.
**Verification**: Test
**Basis**: FIPS 197, FIPS 46-3, RFC 2268 and RFC 6229.

### SYS-CRYPTO-4 HMAC and derived session keys

**Rule**: CryptoAPI HMAC MUST compute RFC 2104 with the selected digest and supplied session key. CryptDeriveKey MUST finalize its base hash and use the documented CSP expansion for AES and triple DES. Plaintext key blobs MUST encode and recover the owned key material and export permissions.
**Verification**: Test
**Basis**: RFC 2104, RFC 4231 and Microsoft CryptDeriveKey documentation.

### SYS-CRYPTO-5 Random output boundaries

**Rule**: Random APIs MUST fill exactly the requested output range. Zero-length random requests MUST succeed without dereferencing a null buffer. Invalid output ranges MUST preserve guest bytes.
**Verification**: Test

### SYS-CRYPTO-6 Secure random source

**Rule**: RtlGenRandom, CryptGenRandom and symmetric CryptGenKey MUST use the shared host OS entropy adapter. They MUST NOT substitute a deterministic generator when that source fails.
**Verification**: Review — platform source selection and cryptographic entropy require review beyond statistical output tests.

### SYS-CRYPTO-7 Binary text conversions

**Rule**: CryptBinaryToString and CryptStringToBinary MUST transform the actual input bytes, including Base64 headers and hexadecimal encodings. Automatic detection MUST report the selected format and header skip count. A/W variants MUST share ASCII format semantics without applying an ANSI code page.
**Verification**: Test
**Basis**: Microsoft CryptBinaryToString and CryptStringToBinary documentation; RFC 4648.

### SYS-CRYPTO-8 Conversion output boundaries

**Rule**: Binary conversion size queries MUST report the complete required output size without writing output buffers. Formatted conversions MUST report ERROR_MORE_DATA and the required size for short buffers. Conversions MUST validate guest output permissions and preserve LastError on success.
**Verification**: Test
**Basis**: Microsoft CryptBinaryToString and CryptStringToBinary documentation.

### SYS-CRYPTO-9 Raw binary copy lengths

**Rule**: Raw CryptStringToBinary output MUST copy the caller's byte capacity from checked guest input, independently of its declared text length. Raw size queries MUST report the input character count, including for W calls. Raw binary-to-string copies MUST omit terminators and preserve the length on insufficient capacity.
**Verification**: Test
**Basis**: Authored native A/W binary conversion controls in `crypt32_encoding_native`.

## Process status

### SYS-PROCESS-1 Process and module queries

**Rule**: PSAPI and K32 aliases MUST query the existing guest process, Loader and section owners. Module queries MUST enforce process query and VM-read rights, including local real handles. Reported paths MUST use owned LDR names or section paths and the same device mapping as QueryDosDevice.
**Verification**: Test

## Detached console

### SYS-CONSOLE-1 Absent console handles

**Rule**: Console input, output and buffer configuration MUST fail with ERROR_INVALID_HANDLE when no console is attached. Redirected standard streams MUST NOT constitute console buffers. Ordinary detached-console failures MUST NOT report capability gaps.
**Verification**: Test

### SYS-CONSOLE-2 Process control handlers

**Rule**: SetConsoleCtrlHandler MUST maintain the process handler table and CTRL+C ignore attribute independently of console attachment. Removing an absent handler MUST fail. FreeConsole MUST succeed while detached and reset process control handlers.
**Verification**: Test
**Basis**: Microsoft FreeConsole and SetConsoleCtrlHandler documentation.

### SYS-CONSOLE-3 Console creation capability

**Rule**: AllocConsole MUST fail and report a capability gap when no host console transport is provided. It MUST NOT publish a console window or valid console handles.
**Verification**: Test

## Installer discovery

### SYS-INSTALL-1 Empty installed state

**Rule**: MSI discovery MUST report the profile's empty installed product and component sets without capability warnings. Queries MUST distinguish unknown products, unknown components and malformed identifiers. Unknown-item failures MUST preserve output buffers and direct-status APIs MUST preserve LastError.
**Verification**: Test

### SYS-INSTALL-2 Installer file hashes

**Rule**: MsiGetFileHash MUST compute the real MD5 of guest file bytes through the existing VFS and coherent file readers. A/W paths MUST share that implementation. Invalid hash structures and failed reads MUST preserve hash output.
**Verification**: Test
**Basis**: Microsoft MsiGetFileHash documentation and RFC 1321.

## Synchronization

### SYS-SYNC-1 Signal and wait ownership

**Rule**: SignalObjectAndWait MUST signal an existing event, release an owned mutex or increment a semaphore before waiting through the shared scheduler. Invalid wait handles MUST leave the signal object unchanged. Alertable waits MUST dispatch queued APCs and report WAIT_IO_COMPLETION.
**Verification**: Test
**Basis**: Microsoft SignalObjectAndWait documentation.

## Profile files

### SYS-PROFILEFILE-1 File-backed INI mutations

**Rule**: Profile queries and writers MUST share the INI parser, VFS layout and coherent file owners. Section replacement MUST preserve unrelated sections. Writers MUST retain UTF-16LE files and encode other files through the guest ANSI code page.
**Verification**: Test
**Basis**: Microsoft GetPrivateProfileSection, WritePrivateProfileSection and WritePrivateProfileString documentation.

### SYS-PROFILEFILE-2 Checksummed profile structures

**Rule**: Profile structure writers MUST store the actual bytes with their checksum. Readers MUST validate size and checksum before writing guest output. Invalid data and inaccessible outputs MUST preserve output bytes.
**Verification**: Test
**Basis**: Microsoft GetPrivateProfileStruct and WritePrivateProfileStruct documentation.

### SYS-PROFILEFILE-3 Native profile writes

**Rule**: Modified native file entries MUST control profile opens, writes, truncation and closes. Failed writes MUST NOT truncate the file. Temporary native handles and guest scratch MUST be released after synchronous completion.
**Verification**: Test

## Local feature queries

### SYS-CORE-1 Win7 local feature queries

**Rule**: GetTickCount64 MUST retain the shared clock's full millisecond count. Legacy processor topology MUST derive from the same single-processor profile as its Ex counterpart. String probes MUST check guest readability, and the native x86 profile MUST report WOW64 directories unavailable without modifying output.
**Verification**: Test
**Basis**: Microsoft GetTickCount64, GetLogicalProcessorInformation, IsBadStringPtr and GetSystemWow64Directory contracts.

## Printing

### SYS-PRINT-1 Empty printer catalog

**Rule**: Local printer enumeration MUST return zero installed or connected printers with checked count outputs. OpenPrinter MUST distinguish the real local server from absent printer devices and enforce ordinary-user access. Printer-device queries MUST fail without fabricating device modes or modifying their output buffers.
**Verification**: Test
**Basis**: Microsoft OpenPrinter, EnumPrinters and DocumentProperties contracts and the configured printer-free profile.

### SYS-REG-1 Shared registry entry points

**Rule**: Legacy, extended, and native registry entry points MUST share keys, names, access checks and deleted-handle state. Enumeration MUST use stored spellings and bounded output buffers. Shell value queries MUST reuse registry encoding and size negotiation.
**Verification**: Test
**Basis**: Microsoft RegEnumKey, RegDeleteKeyEx, NtOpenKey and SHGetValue contracts.

### SYS-STR-1 Counted RTL conversion

**Rule**: Counted ANSI-to-Unicode conversion MUST use the guest ACP and explicit byte length. Insufficient supplied storage MUST preserve the destination descriptor and payload. Allocated strings MUST use the process heap and remain releasable by RtlFreeUnicodeString.
**Verification**: Test
**Basis**: Microsoft RtlAnsiStringToUnicodeString and authored native capacity controls.

### SYS-NLS-1 Portable locale data

**Rule**: Locale identifiers, names, information and enumerators MUST share the guest profile's locale data. Enumeration MUST invoke guest callbacks and honor early termination across yields. Locale queries MUST negotiate encoded output sizes and preserve output on failure.
**Verification**: Test
**Basis**: Microsoft locale name, GetLocaleInfo and enumeration contracts; authored native callback-stop controls.

### SYS-NLS-2 Character classification

**Rule**: ANSI classification MUST classify the decoded Unicode characters using the specified locale's code page. Explicit lengths MUST preserve embedded nulls, and terminated inputs MUST include the null character's classification. Unsupported classification tables MUST fail explicitly without modifying output.
**Verification**: Test
**Basis**: Microsoft GetStringTypeA and GetStringTypeEx contracts.

### SYS-SYNC-2 Extended object creation

**Rule**: Extended event and semaphore creators MUST reuse legacy and native names, signal state, lifetime and wait scheduling. Invalid creation flags MUST fail before publishing a handle. Unsupported access restrictions MUST fail explicitly.
**Verification**: Test
**Basis**: Microsoft CreateEventEx and CreateSemaphoreEx contracts.

### SYS-NLS-3 Localized date and time pictures

**Rule**: Named and LCID date/time formatters MUST share Gregorian date validation, locale names and encoded output negotiation. Date formatting MUST derive the weekday from the supplied date. Failed formatting MUST preserve output bytes.
**Verification**: Test
**Basis**: Microsoft GetDateFormatEx and GetTimeFormatEx contracts; authored fixed-locale native picture controls.

### SYS-CORE-2 UTC to local conversion

**Rule**: UTC-to-local conversion MUST use the guest's configured offset or the caller's validated time-zone rules. Recurring transitions MUST apply the bias in effect before each transition. Conversion MUST preserve output on invalid dates and range overflow.
**Verification**: Test
**Basis**: Microsoft SystemTimeToTzSpecificLocalTime and TIME_ZONE_INFORMATION contracts.

### SYS-SECURITY-1 Guest identity queries

**Rule**: SID and account queries MUST use the existing guest identity and checked SID layouts. Security-descriptor owner queries MUST preserve relative and absolute owner pointers. Privilege-name lookup MUST return stable LUIDs without granting privileges.
**Verification**: Test
**Basis**: Microsoft IsValidSid, LookupAccountSid, LookupPrivilegeValue and GetSecurityDescriptorOwner contracts.

### SYS-SECURITY-2 Ordinary token ownership

**Rule**: Token aliases MUST share privilege state, while DuplicateTokenEx MUST create an independent token copy. AdjustTokenPrivileges MUST preserve absent privileges and report ERROR_NOT_ALL_ASSIGNED without granting authority. Impersonation MUST remain bound to its guest thread, and token process creation MUST enforce the ordinary caller's privilege restrictions.
**Verification**: Test
**Basis**: Microsoft AdjustTokenPrivileges, DuplicateTokenEx, ImpersonateLoggedOnUser and CreateProcessWithTokenW contracts.

### SYS-AUTOMATION-1 Array storage and element ownership

**Rule**: SAFEARRAY creation and indexing MUST use actual guest descriptors, bounds and process-heap storage. Variant array copies MUST retain independent elements through the shared BSTR allocator and real guest COM references. Array destruction MUST respect locks and release owned elements across yielding callbacks.
**Verification**: Test
**Basis**: Microsoft SAFEARRAY and VARIANT contracts; authored native dimension and stride controls.

### SYS-AUTOMATION-2 Scalar conversion ownership and precision

**Rule**: Automation scalar conversions MUST share guest locale data and BSTR ownership. Integer and currency conversions MUST round decimal ties to even without losing integer precision. Failed conversion MUST preserve output bytes and LastError.
**Verification**: Test
**Basis**: Microsoft VarBoolFromStr, VarBstrFromDate, VarCyFromStr and related scalar conversion contracts; authored native scalar controls.

### SYS-AUTOMATION-3 Indirect values and extended coercion

**Rule**: Indirect VARIANT copies MUST resolve borrowed pointers and establish independent ownership before retiring an aliased destination. Extended coercion MUST compute its result before releasing destination resources. Guest reference callbacks MUST preserve conversion continuations across scheduler waits.
**Verification**: Test
**Basis**: Microsoft VariantCopyInd and VariantChangeTypeEx contracts.

### SYS-COM-1 Apartment and server lifetime

**Rule**: COM and OLE initialization MUST maintain nesting and apartment mode per virtual thread. Conflicting modes MUST fail without changing initialization counts. Server references and class-publication suspension MUST belong to the guest process.
**Verification**: Test
**Basis**: Microsoft CoInitializeEx, OleInitialize, CoAddRefServerProcess and CoReleaseServerProcess contracts.

### SYS-COM-2 Guest interface forwarding

**Rule**: OLE drawing, running locks and proxy security MUST invoke the object's actual queried guest interface. Adapters MUST return the method's result and balance acquired references across guest waits. Interface lookup failure MUST NOT manufacture drawing or security effects.
**Verification**: Test
**Basis**: Microsoft OleDraw, OleLockRunning, CoSetProxyBlanket and IUnknown contracts.

### SYS-COM-3 Installed class activation

**Rule**: Class factories MUST activate installed constructors through shared COM identities and reference counts. Multi-interface activation MUST query one constructed object and publish individual HRESULTs. ProgID lookup MUST read the shared virtual registry without inventing classes.
**Verification**: Test
**Basis**: Microsoft CoGetClassObject, CoCreateInstanceEx, CLSIDFromProgID and IClassFactory contracts.

### SYS-AUTOMATION-4 Actual dispatch calls

**Rule**: DispCallFunc MUST invoke the supplied guest procedure or vtable entry with the declared x86 argument widths. Scalar results MUST come from actual EAX, EDX or x87 state after the call. Unsupported aggregate return conventions MUST fail explicitly before executing the procedure.
**Verification**: Test
**Basis**: Microsoft DispCallFunc contract and pinned WinMD x86 scalar layouts.

### SYS-PRINT-2 Printer drawing contexts

**Rule**: Printer DC creation MUST fail ordinarily when no matching printer exists, without creating a GDI handle. Spooling operations MUST reject display, memory and invalid DCs with `SP_ERROR` and `ERROR_INVALID_HANDLE`, without invoking abort callbacks.
**Verification**: Test
**Basis**: Microsoft CreateDC, StartDoc, StartPage, EndPage, EndDoc and SetAbortProc contracts and the configured printer-free profile.

Withdrawn: none
