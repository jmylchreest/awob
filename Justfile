# awob — Justfile
#
# Recipes for build / test / lint / install / release / docs. Modeled
# on the pattern used in jmylchreest/rosec. Run `just` with no args to
# see the default list.

set shell := ["bash", "-cu"]
set positional-arguments

# Workspace metadata. The version in workspace.package is authoritative
# for releases; cargo-release rewrites it.
workspace_version := `grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/'`

# ----------------------------------------------------------------------
# default: show recipes
# ----------------------------------------------------------------------

default:
    @just --list --unsorted

# ----------------------------------------------------------------------
# build / test / lint
# ----------------------------------------------------------------------

# Debug build every binary in the workspace.
build:
    cargo build --workspace --locked

# Release build every binary in the workspace.
build-release:
    cargo build --release --workspace --locked

# Run all workspace tests.
test:
    cargo test --workspace --locked

# Clippy + rustfmt check. Plain clippy (no -D warnings gate) — see
# `.github/workflows/ci.yml` for the rationale; lints are informational
# until the workspace is fully clippy-clean.
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked

# Clippy with --fix + format. Wipes uncommitted changes if you don't
# stage first — guard your work.
lint-fix:
    cargo fmt --all
    cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged

# Type-check without producing artefacts. Faster feedback than `build`.
check:
    cargo check --workspace --locked

# Show every flavour of "what version is this".
version:
    @echo "workspace.package.version: {{workspace_version}}"
    @echo "git describe:               $(git describe --tags --always --dirty 2>/dev/null || echo none)"
    @echo "git commit:                 $(git rev-parse HEAD 2>/dev/null || echo none)"
    @echo "rust toolchain:             $(rustc --version 2>/dev/null || echo none)"

# Wipe target/ and any docusaurus artefacts.
clean:
    cargo clean
    rm -rf docs/build docs/.docusaurus docs/node_modules

# ----------------------------------------------------------------------
# install / uninstall
# ----------------------------------------------------------------------

cargo_bin := env_var_or_default("CARGO_HOME", env_var("HOME") + "/.cargo") + "/bin"

# Comprehensive system-dep install hint for new contributors. Pure
# documentation — recipe just prints. Distro-detect is left to the
# user. Mirrors the apt-get list in .github/workflows/ci.yml.
deps-hint:
    @echo "Debian / Ubuntu:"
    @echo "  sudo apt install build-essential pkg-config \\"
    @echo "    libfontconfig1-dev libfreetype6-dev \\"
    @echo "    libpipewire-0.3-dev libdbus-1-dev \\"
    @echo "    libudev-dev libxkbcommon-dev"
    @echo ""
    @echo "Arch:"
    @echo "  sudo pacman -S base-devel pkgconf fontconfig freetype2 \\"
    @echo "    pipewire dbus libxkbcommon"

# Install every binary to $CARGO_HOME/bin (default ~/.cargo/bin).
install:
    cargo install --path crates/awob-cli --locked
    cargo install --path crates/awob-daemon --locked
    cargo install --path crates/awob-listener-pipewire --locked
    cargo install --path crates/awob-listener-battery --locked
    cargo install --path crates/awob-listener-backlight --locked
    cargo install --path crates/awob-listener-keyboard-backlight --locked
    cargo install --path crates/awob-listener-wob --locked
    @echo "Installed to {{cargo_bin}}"

# Install only the daemon + CLI (skip the optional listeners).
install-min:
    cargo install --path crates/awob-cli --locked
    cargo install --path crates/awob-daemon --locked

# Uninstall every awob binary from $CARGO_HOME/bin.
uninstall:
    -cargo uninstall awob-cli 2>/dev/null
    -cargo uninstall awob-daemon 2>/dev/null
    -cargo uninstall awob-listener-pipewire 2>/dev/null
    -cargo uninstall awob-listener-battery 2>/dev/null
    -cargo uninstall awob-listener-backlight 2>/dev/null
    -cargo uninstall awob-listener-keyboard-backlight 2>/dev/null
    -cargo uninstall awob-listener-wob 2>/dev/null

# ----------------------------------------------------------------------
# demos
# ----------------------------------------------------------------------

# Run a single demo script. Usage: `just demo wedge` / `just demo all`.
demo NAME:
    bash demo/{{NAME}}.sh

# Run every demo in sequence.
demo-all:
    bash demo/all.sh

# ----------------------------------------------------------------------
# dependencies
# ----------------------------------------------------------------------

# Show outdated direct dependencies (workspace).
deps-outdated:
    cargo outdated --workspace --root-deps-only

# Bump every dependency to the latest compatible version (--locked).
deps-update:
    cargo update --workspace

# Bump every dependency to the latest version, breaking-change-aware.
# Requires cargo-edit.
deps-upgrade:
    cargo upgrade --workspace --locked

# ----------------------------------------------------------------------
# release
# ----------------------------------------------------------------------

# Patch / minor / major releases via cargo-release. Pass `push` as the
# argument to also push the resulting commit + tag and trigger the
# release workflow.
release-patch *args:
    just _release patch {{args}}

release-minor *args:
    just _release minor {{args}}

release-major *args:
    just _release major {{args}}

# Pre-release: `just release-rc 0.1.0-rc.1 push`.
release-rc VERSION *args:
    just _release {{VERSION}} {{args}}

# Internal: bump + tag (signed) via cargo-release. Push if `push` arg.
_release VERSION *args:
    @command -v cargo-release >/dev/null || (echo 'install cargo-release: cargo install cargo-release' && exit 1)
    cargo release {{VERSION}} --no-publish --no-confirm --execute
    @[[ "{{args}}" == *push* ]] && git push origin HEAD && git push origin --tags || echo "Tagged locally; pass 'push' to publish."

# ----------------------------------------------------------------------
# docs (Docusaurus)
# ----------------------------------------------------------------------

# Run docusaurus dev server (auto-reloads on edit).
docs-dev:
    cd docs && npm install && npm start

# Build docusaurus production site (output: docs/build).
docs-build:
    cd docs && npm install && npm run build

# Type-check docusaurus config + content without building.
docs-check:
    cd docs && npm install && npm run typecheck
