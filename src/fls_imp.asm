; Override kernel32.dll import thunks for Vista+ APIs with pointers to
; our XP-compatible wrappers in fls_shim.c
;
; The MSVC CRT calls these through __imp_ thunks (__declspec(dllimport)).
; By providing these symbols here, the linker uses our function pointers
; instead of creating kernel32.dll imports (which would fail on XP).

.386
.model flat

; Our C wrapper functions (stdcall decorated names)
EXTERN _xp_FlsAlloc@4:PROC
EXTERN _xp_FlsFree@4:PROC
EXTERN _xp_FlsGetValue@4:PROC
EXTERN _xp_FlsSetValue@8:PROC
EXTERN _xp_InitializeCriticalSectionEx@12:PROC

.data

; FLS → TLS redirects
PUBLIC __imp__FlsAlloc@4
__imp__FlsAlloc@4 DD _xp_FlsAlloc@4

PUBLIC __imp__FlsFree@4
__imp__FlsFree@4 DD _xp_FlsFree@4

PUBLIC __imp__FlsGetValue@4
__imp__FlsGetValue@4 DD _xp_FlsGetValue@4

PUBLIC __imp__FlsSetValue@8
__imp__FlsSetValue@8 DD _xp_FlsSetValue@8

; InitializeCriticalSectionEx → InitializeCriticalSectionAndSpinCount
PUBLIC __imp__InitializeCriticalSectionEx@12
__imp__InitializeCriticalSectionEx@12 DD _xp_InitializeCriticalSectionEx@12

END
