# TOTP replay state

An accepted TOTP step is recorded both in the encrypted wallet and in a
host-local record keyed by a hash of the authenticator secret. Wallet copies
using the same authenticator on the same host share that record. Enrollment,
unlock, verification, and disable consume steps; reusing a step is rejected.

The host record defaults to `$XDG_STATE_HOME/vtorrent/totp-replay` or
`$HOME/.local/state/vtorrent/totp-replay`; on Windows it uses
`%LOCALAPPDATA%/vtorrent/totp-replay`. `VTORRENT_TOTP_STATE_DIR` may specify an
absolute alternative. Protect that directory and retain it when moving a
wallet between paths. An unreadable or corrupt record fails closed.

This is **not** a distributed replay guarantee. A copy on another machine,
or restoring both the wallet and host state to an older snapshot, can roll
back replay history. Re-enroll 2FA after restoration or use a shared trusted
service if cross-machine replay prevention is required. TOTP gates application
operations; it is not an independent factor in wallet-file encryption.
