; Override bcrypt.dll import thunk for BCryptGenRandom with pointer to
; our XP-compatible wrapper in bcrypt_shim.c
;
; Ring (used by rustls) calls BCryptGenRandom through __imp_ thunk.
; By providing this symbol here, the linker uses our function pointer
; instead of creating a bcrypt.dll import (which would fail on XP).

.386
.model flat

; Our C wrapper function (stdcall decorated name)
EXTERN _xp_BCryptGenRandom@16:PROC

.data

; BCryptGenRandom → xp_BCryptGenRandom (CryptGenRandom wrapper)
PUBLIC __imp__BCryptGenRandom@16
__imp__BCryptGenRandom@16 DD _xp_BCryptGenRandom@16

END
