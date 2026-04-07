# ADR-002: API Key Storage — Secret Service with File Fallback

## Status

Accepted

## Context

Gemini Lite requires a Google AI API key. This key grants access to a paid (or rate-limited free) service. Leaking it would allow unauthorized usage.

The app targets Linux desktops, where the Secret Service D-Bus API (implemented by GNOME Keyring, KeePassXC, KDE Wallet) is the standard for credential storage. However, not all users run a Secret Service daemon (headless setups, minimal WMs, containers).

### Alternatives Considered

| Approach | Pros | Cons |
|---|---|---|
| **Environment variable only** | Simple, 12-factor compliant | Poor UX for desktop users. Requires manual `.bashrc` editing. Lost on reboot if not persisted. |
| **Secret Service only** | OS-level encryption, standard API | Fails on systems without a running daemon (Sway without keyring, SSH sessions, containers). |
| **Application-level encryption (AES)** | Works everywhere | Where do you store the encryption key? Turtles all the way down. Adds complexity without real security gain for a local desktop app. |
| **Secret Service + file fallback** | Best of both worlds: secure when possible, functional always | The fallback file is plaintext (mode 0600). An attacker with local file access could read it. |

## Decision

Use a **tiered approach**:

1. **Load order:** `GEMINI_API_KEY` env var → GNOME Keyring (via `keyring` crate) → `~/.config/gemini-lite/api-key` file.
2. **Save order:** Try Secret Service first. If the daemon is unavailable, fall back to writing a mode-`0600` file under `XDG_CONFIG_HOME`.

No application-level encryption is applied to the fallback file. The reasoning: if an attacker has read access to `~/.config/` with the user's UID, they already have access to the user's browser cookies, SSH keys, and GPG keyring. Adding AES encryption with a hardcoded or derived key would be security theater.

## Consequences

- **Positive:** On GNOME/KDE desktops (the primary target), the key is stored in the system keyring with OS-level encryption.
- **Positive:** The app works out of the box on minimal setups without requiring a keyring daemon.
- **Positive:** Power users can use the env var for scripting or CI.
- **Risk:** The fallback file is readable by the local user. This is documented and accepted as equivalent to the threat model of `~/.ssh/id_rsa`.
- **Trade-off:** The `keyring` crate adds a dependency on D-Bus libraries at compile time. This is already satisfied by the GTK dependency chain.
