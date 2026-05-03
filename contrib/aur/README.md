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
are filled in at release time. Until release-workflow automation
lands ([FUTURES.md](../../FUTURES.md)), render the file by hand
when bumping:

```sh
VERSION=0.0.1
SHA256=$(curl -sL "https://github.com/jmylchreest/awob/releases/download/v${VERSION}/awob-${VERSION}-x86_64-unknown-linux-gnu.tar.gz.sha256" | cut -d' ' -f1)
sed -e "s/@VERSION@/${VERSION}/" -e "s/@SHA256@/${SHA256}/" \
    PKGBUILD-bin > /tmp/awob-bin/PKGBUILD
```

Then push to AUR via your usual workflow (`makepkg --printsrcinfo`
into `.SRCINFO`, commit, push to the AUR remote).

`PKGBUILD-git` derives its `pkgver` at build time, so it doesn't
need any rendering — `makepkg --noconfirm` works as-is.
