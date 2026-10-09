#!/usr/bin/env python3
"""Validate release recipes and publish them with ordinary fast-forward Git pushes."""

import argparse
import contextlib
import hashlib
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile


PACKAGES = {
    "awob-bin": "PKGBUILD-bin",
    **{f"awob-listener-{name}-bin": f"PKGBUILD-listener-{name}-bin" for name in (
        "pipewire", "battery", "backlight", "keyboard-backlight", "power-profile", "wob"
    )},
    "awob-listeners-all": "PKGBUILD-listeners-all-bin",
    "awob-git": "PKGBUILD-git",
}
ROOT = Path(__file__).resolve().parents[2]


def run(*args, cwd=None, env=None):
    result = subprocess.run(args, cwd=cwd, env=env, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(f"{args[0]} failed ({result.returncode}): {result.stderr.strip()}")
    return result.stdout.strip()


def version_from_tag(tag):
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError("AUR publication requires a stable tag such as v0.1.9")
    return tag[1:]


def fields(srcinfo):
    result = {}
    for line in srcinfo.splitlines():
        if " = " in line:
            key, value = line.strip().split(" = ", 1)
            result.setdefault(key, []).append(value)
    return result


def validate_metadata(srcinfo, package, version):
    data = fields(srcinfo)
    if data.get("pkgbase") != [package] or data.get("pkgname") != [package]:
        raise ValueError(f"Package identity mismatch: {package}")
    if package != "awob-git" and data.get("pkgver") != [version]:
        raise ValueError(f"Version mismatch: {package}")


def check_pin(pin):
    if not pin.is_file() or not any(
        line.startswith("aur.archlinux.org ssh-ed25519 ")
        for line in pin.read_text().splitlines()
    ):
        raise ValueError(f"Missing pinned AUR host key: {pin}")


def prepare(tag, assets, output, source=ROOT):
    version = version_from_tag(tag)
    check_pin(source / ".github/aur_known_hosts")
    archive = f"awob-{version}-x86_64-unknown-linux-gnu.tar.gz"
    checksum = hashlib.sha256((assets / archive).read_bytes()).hexdigest()
    listed = (assets / (archive + ".sha256")).read_text().split()
    if listed != [checksum, archive]:
        raise ValueError("Release archive checksum or filename mismatch")
    for package, template in PACKAGES.items():
        expected = subprocess.check_output([
            "git", "show", f"{tag}:contrib/aur/{template}"
        ], cwd=source).decode()
        if package != "awob-git":
            expected = expected.replace("@VERSION@", version).replace("@SHA256@", checksum)
        recipe = assets / f"awob-{version}.{package}.PKGBUILD"
        if recipe.read_text() != expected:
            raise ValueError(f"Release recipe differs from tagged source: {package}")
        destination = output / package
        destination.mkdir(parents=True, exist_ok=True)
        (destination / "PKGBUILD").write_text(expected)
        run("bash", "-n", "PKGBUILD", cwd=destination)
        srcinfo = run("makepkg", "--printsrcinfo", cwd=destination)
        validate_metadata(srcinfo, package, version)
        (destination / ".SRCINFO").write_text(srcinfo + "\n")
        print(f"Validated {package}", flush=True)


@contextlib.contextmanager
def ssh_environment(pin, key):
    check_pin(pin)
    if not key.strip():
        raise ValueError("AUR_KEY is required for publication")
    with tempfile.TemporaryDirectory(prefix="awob-aur-key-") as directory:
        identity = Path(directory) / "key"
        identity.write_text(key.rstrip() + "\n")
        identity.chmod(0o600)
        command = [
            "ssh", "-F", "/dev/null", "-i", str(identity),
            "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=20", "-o", "StrictHostKeyChecking=yes",
            "-o", "HostKeyAlgorithms=ssh-ed25519",
            "-o", f"UserKnownHostsFile={pin.resolve()}",
            "-o", "GlobalKnownHostsFile=/dev/null", "-o", "UpdateHostKeys=no",
        ]
        env = dict(os.environ, GIT_SSH_COMMAND=shlex.join(command))
        env.pop("AUR_KEY", None)
        yield env


def release_number(srcinfo):
    data = fields(srcinfo)
    version = data["pkgver"][0]
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError(f"Refusing to replace an unrecognized remote version: {version}")
    return (int(data.get("epoch", ["0"])[0]),
            tuple(map(int, version.split("."))),
            tuple(map(int, data["pkgrel"][0].split("."))))


def publish_package(package, version, staged, remote, env):
    recipe = (staged / "PKGBUILD").read_text()
    srcinfo = (staged / ".SRCINFO").read_text()
    validate_metadata(srcinfo, package, version)
    with tempfile.TemporaryDirectory(prefix="awob-aur-repo-") as directory:
        repo = Path(directory)
        run("git", "init", "--initial-branch=master", str(repo), env=env)
        run("git", "remote", "add", "origin", remote, cwd=repo, env=env)
        advertised = run("git", "ls-remote", "origin", "HEAD", "refs/heads/master", cwd=repo, env=env)
        refs = {ref: sha for sha, ref in (line.split() for line in advertised.splitlines())}
        # Fetch an explicit branch, never rely on the server's symbolic HEAD.
        # Older AUR repositories can advertise only a detached HEAD.
        base = "refs/heads/master" if "refs/heads/master" in refs else "HEAD" if "HEAD" in refs else None
        if base:
            run("git", "fetch", "origin", base, cwd=repo, env=env)
            run("git", "checkout", "-B", "master", "FETCH_HEAD", cwd=repo, env=env)
        old_recipe = (repo / "PKGBUILD").read_text() if (repo / "PKGBUILD").exists() else None
        old_info = (repo / ".SRCINFO").read_text() if (repo / ".SRCINFO").exists() else None
        if old_info and package != "awob-git":
            old_version, new_version = release_number(old_info), release_number(srcinfo)
            if old_version > new_version:
                raise ValueError(f"Refusing to downgrade {package}")
            if old_version == new_version and old_recipe != recipe:
                raise ValueError(f"Refusing to replace the existing {package} release; bump its version")
        # VCS versions come from pkgver() at install time. Do not make an empty
        # release commit merely because the metadata tool formats it differently.
        unchanged = old_recipe == recipe and (package == "awob-git" or old_info == srcinfo)
        if not unchanged:
            (repo / "PKGBUILD").write_text(recipe)
            (repo / ".SRCINFO").write_text(srcinfo)
            run("git", "add", "PKGBUILD", ".SRCINFO", cwd=repo, env=env)
            run("git", "-c", "user.name=jmylchreest", "-c", "user.email=jmylchreest@gmail.com",
                "-c", "commit.gpgsign=false", "commit", "-m", f"release: v{version}", cwd=repo, env=env)
        commit = run("git", "rev-parse", "HEAD", cwd=repo, env=env)
        if not unchanged or "refs/heads/master" not in refs:
            # Concurrent updates must fail rather than rewrite remote history.
            run("git", "push", "origin", "HEAD:refs/heads/master", cwd=repo, env=env)
        actual = run("git", "ls-remote", "origin", "refs/heads/master", cwd=repo, env=env)
        if actual.split()[0] != commit:
            raise RuntimeError(f"Remote verification failed: {package}")
        return ("unchanged" if unchanged else "published"), commit


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    preflight = sub.add_parser("prepare")
    preflight.add_argument("--tag", required=True)
    preflight.add_argument("--assets", type=Path, default=Path("dist"))
    preflight.add_argument("--output", type=Path, default=Path("aur-ready"))
    publish = sub.add_parser("publish")
    publish.add_argument("--tag", required=True)
    publish.add_argument("--package", choices=PACKAGES, required=True)
    publish.add_argument("--staged", type=Path, default=Path("aur-ready"))
    args = parser.parse_args()
    if args.command == "prepare":
        prepare(args.tag, args.assets.resolve(), args.output.resolve())
    else:
        version = version_from_tag(args.tag)
        key = os.environ.pop("AUR_KEY", "")
        with ssh_environment(ROOT / ".github/aur_known_hosts", key) as env:
            status, commit = publish_package(args.package, version, args.staged / args.package,
                f"ssh://aur@aur.archlinux.org/{args.package}.git", env)
        message = f"{args.package}: {status}, verified remote master at {commit}"
        print(message)
        if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(summary, "a") as output:
                output.write(message + "\n")


if __name__ == "__main__":
    main()
