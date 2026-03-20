// XP compatibility shim for Vista+ APIs used by VS2022's static CRT (libcmt.lib)
// The companion xp_imp.asm overrides __imp_ import thunks to point here,
// preventing the linker from importing these from kernel32.dll.

#include <windows.h>

// --- FLS → TLS (FlsAlloc etc. are Vista+, TlsAlloc etc. are XP-safe) ---

DWORD WINAPI xp_FlsAlloc(PVOID lpCallback) {
    (void)lpCallback; // TLS doesn't support cleanup callbacks
    return TlsAlloc();
}

BOOL WINAPI xp_FlsFree(DWORD dwFlsIndex) {
    return TlsFree(dwFlsIndex);
}

PVOID WINAPI xp_FlsGetValue(DWORD dwFlsIndex) {
    return TlsGetValue(dwFlsIndex);
}

BOOL WINAPI xp_FlsSetValue(DWORD dwFlsIndex, PVOID lpFlsData) {
    return TlsSetValue(dwFlsIndex, lpFlsData);
}

// --- InitializeCriticalSectionEx → InitializeCriticalSectionAndSpinCount ---
// InitializeCriticalSectionEx is Vista+, adds a Flags param (e.g. NO_DEBUG_INFO).
// On XP we ignore Flags and use the XP-compatible AndSpinCount variant.

BOOL WINAPI xp_InitializeCriticalSectionEx(
    LPCRITICAL_SECTION lpCriticalSection,
    DWORD dwSpinCount,
    DWORD Flags) {
    (void)Flags;
    return InitializeCriticalSectionAndSpinCount(lpCriticalSection, dwSpinCount);
}
