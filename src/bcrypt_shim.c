/*
 * bcrypt shim for Windows XP
 *
 * Ring (used by rustls) imports BCryptGenRandom from bcrypt.dll (Vista+).
 * This shim provides BCryptGenRandom using XP-compatible CryptGenRandom
 * from advapi32.dll, so no bcrypt.dll is needed at runtime.
 *
 * Linked statically into hoptodesk.exe via build.rs.
 */

#include <windows.h>
#include <wincrypt.h>

static HCRYPTPROV g_hProv = 0;
static BOOL g_initialized = FALSE;

static BOOL EnsureInit(void) {
    if (g_initialized) return TRUE;
    if (CryptAcquireContextW(&g_hProv, NULL, NULL, PROV_RSA_FULL, CRYPT_VERIFYCONTEXT)) {
        g_initialized = TRUE;
        return TRUE;
    }
    return FALSE;
}

/* This is the XP-compatible implementation that ring will call */
long __stdcall xp_BCryptGenRandom(
    void *hAlgorithm,
    unsigned char *pbBuffer,
    unsigned long cbBuffer,
    unsigned long dwFlags
) {
    (void)hAlgorithm;
    (void)dwFlags;

    if (pbBuffer == NULL && cbBuffer > 0)
        return (long)0xC000000DL;
    if (cbBuffer == 0)
        return 0;
    if (!EnsureInit())
        return (long)0xC0000001L;
    if (CryptGenRandom(g_hProv, cbBuffer, pbBuffer))
        return 0;
    return (long)0xC0000001L;
}
