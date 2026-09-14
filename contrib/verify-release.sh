#!/usr/bin/env bash
# Check the downloaded payloads before either publisher can run.
set -euo pipefail

: "${VERSION:?VERSION must identify the staged release}"
target=x86_64-unknown-linux-gnu
archive="awob-${VERSION}-${target}"
(cd dist && sha256sum -c "${archive}.tar.gz.sha256")

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
tar -xzf "dist/${archive}.tar.gz" -C "$scratch"
for binary in awob awob-daemon awob-listener-{pipewire,battery,backlight,keyboard-backlight,power-profile,wob}; do
    test -x "$scratch/$archive/bin/$binary"
done
test -f "$scratch/$archive/lib/systemd/user/awob.service"
test -d "$scratch/$archive/share/awob/themes"

for package in awob-bin awob-listener-{pipewire,battery,backlight,keyboard-backlight,power-profile,wob}-bin awob-listeners-all awob-git; do
    staged="aur-staged/$package/PKGBUILD"
    attachment="dist/awob-${VERSION}.${package}.PKGBUILD"
    test -s "$staged"
    bash -n "$staged"
    cmp "$staged" "$attachment"
    if [[ "$package" != awob-git ]]; then
        # Template comments can describe placeholders; executable fields cannot retain them.
        if sed '/^[[:space:]]*#/d' "$staged" | grep -Eq '@(VERSION|SHA256)@'; then
            echo "Unrendered placeholder in $staged" >&2
            exit 1
        fi
    fi
done
