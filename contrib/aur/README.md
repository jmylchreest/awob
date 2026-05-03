# AUR packaging

awob ships eight AUR packages: a slim "core" binary package, one
binary package per official listener, a meta-package that pulls every
listener at once, and a from-source VCS build.

| Package | Source file | What it installs |
|---|---|---|
| `awob-bin` | `PKGBUILD-bin` | Daemon + CLI + stock themes + systemd user unit. The minimum viable install. |
| `awob-listener-pipewire-bin` | `PKGBUILD-listener-pipewire-bin` | PipeWire volume / mute listener. |
| `awob-listener-battery-bin` | `PKGBUILD-listener-battery-bin` | Battery + AC state listener. |
| `awob-listener-backlight-bin` | `PKGBUILD-listener-backlight-bin` | Display backlight listener. |
| `awob-listener-keyboard-backlight-bin` | `PKGBUILD-listener-keyboard-backlight-bin` | Keyboard backlight listener. |
| `awob-listener-wob-bin` | `PKGBUILD-listener-wob-bin` | wob-protocol FIFO bridge. |
| `awob-listeners-all` | `PKGBUILD-listeners-all-bin` | Meta-package — depends on every `awob-listener-*-bin`. No payload. |
| `awob-git` | `PKGBUILD-git` | From-source kitchen-sink build of `main`. Installs daemon + CLI + every official listener + themes + systemd unit. |

Pick the slice that matches how you use awob:

```sh
# Minimum: just the daemon + CLI.
yay -S awob-bin

# Daemon + every listener (the most common ask).
yay -S awob-bin awob-listeners-all

# Daemon + a specific listener subset.
yay -S awob-bin awob-listener-pipewire-bin awob-listener-battery-bin

# Track main from source.
yay -S awob-git
```

After install, enable the user unit (only `awob-bin` and `awob-git`
ship it; the listener packages are pure binary drop-ins that the
daemon's supervisor auto-discovers on `PATH`):

```sh
systemctl --user daemon-reload
systemctl --user enable --now awob.service
```

`awob-git` `provides=` and `conflicts=` cover all the binary packages
it would otherwise overlap with, so you can swap from `awob-bin +
awob-listeners-all` to `awob-git` (or back) with a single `pacman
-S` and pacman will resolve it cleanly.

## Maintainer notes

`PKGBUILD-bin` and each `PKGBUILD-listener-*-bin` carry `@VERSION@`
and `@SHA256@` placeholders that the release workflow fills in
automatically when a `v*` tag is pushed. The rendered files ride
along on the GitHub release as
`awob-<version>.<pkgname>.PKGBUILD` and are pushed to AUR by the
matrixed `aur-publish` workflow job, gated on the repository secret
`AUR_KEY` (an SSH private key whose public half is registered with
aur.archlinux.org under the maintainer's account).

`PKGBUILD-listeners-all-bin` is a payload-free meta-package whose
sole job is to pull every `awob-listener-*-bin` as a dependency.

`PKGBUILD-git` derives its `pkgver` at build time and is shipped
verbatim — no placeholder rendering needed.

To render a binary PKGBUILD manually for a one-off (e.g. testing a
release tarball before tagging):

```sh
VERSION=0.0.1
SHA256=$(curl -sL "https://github.com/jmylchreest/awob/releases/download/v${VERSION}/awob-${VERSION}-x86_64-unknown-linux-gnu.tar.gz.sha256" | cut -d' ' -f1)
sed -e "s/@VERSION@/${VERSION}/" -e "s/@SHA256@/${SHA256}/" \
    PKGBUILD-bin > /tmp/awob-bin/PKGBUILD
```
