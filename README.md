# HopToDesk for Windows XP

### Free, Secure Remote Desktop for Legacy Systems

![HopToDesk on Windows XP](https://www.hoptodesk.com/img/hoptodesk-windows-xp.png)

HopToDesk for Windows XP is a free, fast, and lightweight remote desktop application designed to run on **Windows XP SP2 and later, both 32-bit and 64-bit**. Built in Rust for performance and reliability, it brings modern remote access capabilities to legacy Windows systems that most remote desktop software no longer supports.

This is a separate project from the main [HopToDesk project on GitLab](https://gitlab.com/hoptodesk/hoptodesk) due to the unique requirements of maintaining software on older versions of Windows. For the full cross-platform version (Windows 7+, macOS, Linux), visit [hoptodesk.com](https://www.hoptodesk.com).

## Features

- **End-to-End Encryption** — Curve25519 key exchange + XSalsa20-Poly1305 authenticated encryption with Ed25519 signing. Wire-compatible with the standard HopToDesk client.
- **Remote Desktop Control** — Low-latency screen sharing with keyboard and mouse input, multi-monitor support, and aspect ratio preservation.
- **File Transfer** — Bidirectional file and folder transfer with progress tracking, hidden file support, and directory browsing.
- **Chat** — Real-time text chat during remote sessions via a separate floating window.
- **Clipboard Sync** — Automatic bidirectional clipboard text synchronization with compression.
- **Wake on LAN** — Send magic packets to wake sleeping machines on your network. MAC address stored per peer.
- **Unattended Access** — Set a permanent password for always-on remote access without user interaction.
- **44 Languages** — Runtime language switching with no restart required. Supports: Arabic, Catalan, Chinese, Croatian, Czech, Danish, Dutch, English, Esperanto, Estonian, Basque, Finnish, French, German, Greek, Hebrew, Hungarian, Indonesian, Italian, Japanese, Kazakh, Korean, Latvian, Lithuanian, Norwegian, Persian, Polish, Portuguese, Brazilian Portuguese, Romanian, Russian, Serbian, Slovak, Slovenian, Albanian, Spanish, Swedish, Thai, Turkish, Taiwanese, Ukrainian, Vietnamese, Belarusian, and Bulgarian.

## Requirements

- Windows XP SP2 or later (32-bit or 64-bit; runs on Windows XP Professional x64 Edition via WOW64)
- Minimal RAM and disk usage
- No installation required — runs as a portable executable

## Download

Get the latest version at [hoptodesk.com](https://www.hoptodesk.com/)

## Build

### Prerequisites

- Windows 10/11 host
- Visual Studio 2022 with the "Desktop development with C++" workload, including the x86 (32-bit) MSVC toolset and the Windows 10 SDK
- [rust9x](https://github.com/rust9x/rust) toolchain — download `rust9x.7z` from the [rust9x releases](https://github.com/rust9x/rust/releases), extract, and register it with rustup:

  ```
  rustup toolchain link rust9x <path-to-extracted-rust9x>
  ```

Do not run `cargo update`. The checked-in `Cargo.lock` is pinned to versions that build under rust9x; if it has been regenerated, restore it with `git checkout -- Cargo.lock`.

### Build (main exe)

```
cargo +rust9x build --release -p hoptodesk --target i686-rust9x-windows-msvc
```

Output: `target\i686-rust9x-windows-msvc\release\hoptodesk.exe`.

