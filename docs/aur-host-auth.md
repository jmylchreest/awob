# AUR SSH host authentication

Release publication trusts the Ed25519 key in `.github/aur_known_hosts`. It was copied from the [Arch Linux infrastructure repository](https://github.com/archlinux/infrastructure/blob/15f83e8f0ef32183b9dcc928ed4e0ac400aaf5a5/docs/ssh-known_hosts.txt), not from the publishing connection. Its SHA256 fingerprint is `RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4`.

The pre-alignment step uses that file with strict host checking. The pinned deploy action runs in Docker and performs its own key scan; `GIT_SSH_COMMAND` makes its Git operations ignore scanned keys and use the checked-in pin at `/github/workspace/.github/aur_known_hosts`. Both paths disable additional global host files and automatic key updates, and require Ed25519.

A host-key rotation stops publication. Verify the replacement against Arch Linux's independently published infrastructure records, then update the pin and fingerprint in a reviewed PR. Never fix a failed check by accepting a fresh network scan.

Validation can inspect the fingerprint with `ssh-keygen -lf .github/aur_known_hosts -E sha256` and effective options with `ssh -G`. This change does not run an AUR publication as a test.
