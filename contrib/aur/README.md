# AUR packaging

Two PKGBUILDs:

| File | What it builds | When to use |
|---|---|---|
| `PKGBUILD-bin` | `awob-bin` | Pulls the release tarball from GitHub. Fast install, no Rust toolchain needed. |
| `PKGBUILD-git` | `awob-git` | Builds from the latest `main` via cargo. Tracks development. |

Both install:

* Binaries to `/usr/bin/`.
* Stock themes + palettes to `/usr/share/awob/themes/`.
* The systemd user unit to `/usr/lib/systemd/user/awob.service`.
* `LICENSE` + `README.md` to standard doc paths.

After install, enable the user unit:

```sh
systemctl --user daemon-reload
systemctl --user enable --now awob.service
```

## Maintainer notes

`PKGBUILD-bin` carries `@VERSION@` and `@SHA256@` placeholders that
the release workflow fills in automatically when a `v*` tag is
pushed. The rendered PKGBUILD is uploaded as a release asset
(`awob-<version>.aur-bin.PKGBUILD`) and pushed to AUR by the
`aur-publish-bin` workflow job, gated on the repository secret
`AUR_KEY` (an SSH private key whose public half is registered with
aur.archlinux.org under the maintainer's account).

`PKGBUILD-git` derives its `pkgver` at build time, so it doesn't
need any rendering — the workflow ships it verbatim.

To render manually for a one-off (e.g. testing a release tarball
before tagging):

```sh
VERSION=0.0.1
SHA256=$(curl -sL "https://github.com/jmylchreest/awob/releases/download/v${VERSION}/awob-${VERSION}-x86_64-unknown-linux-gnu.tar.gz.sha256" | cut -d' ' -f1)
sed -e "s/@VERSION@/${VERSION}/" -e "s/@SHA256@/${SHA256}/" \
    PKGBUILD-bin > /tmp/awob-bin/PKGBUILD
```
