import hashlib
import os
from pathlib import Path
import shutil
import tempfile
import unittest

import publish


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
                        GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.com",
                        GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.com")
        self.remote = self.root / "remote.git"
        publish.run("git", "init", "--bare", str(self.remote), env=self.env)
        self.stage = self.root / "stage"
        self.stage.mkdir()
        self.recipe("0.1.9")

    def recipe(self, version, extra=""):
        (self.stage / "PKGBUILD").write_text(
            f"pkgname=awob-bin\npkgver={version}\npkgrel=1\n"
            "pkgdesc='Test package'\narch=('any')\nlicense=('MIT')\npackage() { :; }\n" + extra)
        info = publish.run("makepkg", "--printsrcinfo", cwd=self.stage, env=self.env)
        (self.stage / ".SRCINFO").write_text(info + "\n")

    def push(self, version="0.1.9"):
        return publish.publish_package("awob-bin", version, self.stage, str(self.remote), self.env)

    def head(self):
        return publish.run("git", "--git-dir", str(self.remote), "rev-parse", "master", env=self.env)

    def test_first_publication_and_repeat_are_idempotent(self):
        status, commit = self.push()
        self.assertEqual(status, "published")
        self.assertEqual(self.head(), commit)
        self.assertEqual(self.push(), ("unchanged", commit))
        self.assertEqual(publish.run("git", "--git-dir", str(self.remote), "rev-list", "--count", "master"), "1")

    def test_detached_remote_head_does_not_require_force(self):
        _, old = self.push()
        (self.remote / "HEAD").write_text(old + "\n")
        self.recipe("0.1.10")
        _, new = self.push("0.1.10")
        self.assertNotEqual(old, new)
        self.assertEqual(publish.run("git", "--git-dir", str(self.remote), "rev-parse", "master^"), old)

    def test_legacy_head_without_master_is_preserved(self):
        _, old = self.push()
        (self.remote / "HEAD").write_text(old + "\n")
        publish.run("git", "--git-dir", str(self.remote), "update-ref", "-d", "refs/heads/master")
        self.assertEqual(self.push(), ("unchanged", old))
        self.assertEqual(self.head(), old)

    def test_unchanged_vcs_recipe_does_not_publish_metadata_churn(self):
        recipe = self.stage / "PKGBUILD"
        recipe.write_text(recipe.read_text().replace("awob-bin", "awob-git"))
        info = self.stage / ".SRCINFO"
        info.write_text(publish.run("makepkg", "--printsrcinfo", cwd=self.stage) + "\n")
        _, old = publish.publish_package("awob-git", "0.1.9", self.stage, str(self.remote), self.env)
        info.write_text(info.read_text() + "# generator formatting changed\n")
        result = publish.publish_package("awob-git", "0.1.10", self.stage, str(self.remote), self.env)
        self.assertEqual(result, ("unchanged", old))

    def test_missing_remote_is_not_treated_as_an_empty_repository(self):
        with self.assertRaises(RuntimeError):
            publish.publish_package("awob-bin", "0.1.9", self.stage, str(self.root / "missing.git"), self.env)

    def test_downgrade_is_rejected(self):
        _, old = self.push()
        self.recipe("0.1.8")
        with self.assertRaisesRegex(ValueError, "downgrade"):
            self.push("0.1.8")
        self.assertEqual(self.head(), old)

    def test_same_version_with_different_recipe_is_rejected(self):
        _, old = self.push()
        self.recipe("0.1.9", "# changed release\n")
        with self.assertRaisesRegex(ValueError, "existing"):
            self.push()
        self.assertEqual(self.head(), old)

    def test_rejected_push_fails_without_overwriting_remote(self):
        _, old = self.push()
        hook = self.remote / "hooks/pre-receive"
        hook.write_text("#!/bin/sh\nexit 1\n")
        hook.chmod(0o755)
        self.recipe("0.1.10")
        with self.assertRaises(RuntimeError):
            self.push("0.1.10")
        self.assertEqual(self.head(), old)

    def test_concurrent_remote_update_is_not_overwritten(self):
        _, old = self.push()
        tree = publish.run("git", "--git-dir", str(self.remote), "rev-parse", "master^{tree}")
        hooks = self.root / "hooks"
        hooks.mkdir()
        hook = hooks / "pre-push"
        import sys
        hook.write_text(f"#!{sys.executable}\n" +
            "import subprocess\n" +
            f"git = ['git', '--git-dir', {str(self.remote)!r}]\n" +
            f"new = subprocess.check_output(git + ['commit-tree', {tree!r}, '-p', {old!r}], input=b'Concurrent update\\n').decode().strip()\n" +
            "subprocess.check_call(git + ['update-ref', 'refs/heads/master', new])\n")
        hook.chmod(0o755)
        self.env.update(GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="core.hooksPath", GIT_CONFIG_VALUE_0=str(hooks))
        self.recipe("0.1.10")
        with self.assertRaises(RuntimeError):
            self.push("0.1.10")
        self.assertNotEqual(self.head(), old)
        self.assertEqual(publish.run("git", "--git-dir", str(self.remote), "log", "-1", "--format=%s"), "Concurrent update")

    def test_wrong_package_identity_is_rejected(self):
        info = self.stage / ".SRCINFO"
        info.write_text(info.read_text().replace("awob-bin", "unrelated-package"))
        with self.assertRaisesRegex(ValueError, "identity"):
            self.push()

    def test_missing_checkout_pin_fails_before_ssh_setup(self):
        with self.assertRaisesRegex(ValueError, "host key"):
            with publish.ssh_environment(self.root / "missing", "not-a-real-key"):
                self.fail("Missing pin was accepted")

    def test_ssh_configuration_is_pinned_and_key_is_cleaned_up(self):
        pin = publish.ROOT / ".github/aur_known_hosts"
        with publish.ssh_environment(pin, "not-a-real-key") as env:
            import shlex
            args = shlex.split(env["GIT_SSH_COMMAND"])
            key = Path(args[args.index("-i") + 1])
            self.assertEqual(key.stat().st_mode & 0o777, 0o600)
            self.assertIn("StrictHostKeyChecking=yes", args)
            self.assertIn(f"UserKnownHostsFile={pin.resolve()}", args)
            self.assertNotIn("AUR_KEY", env)
        self.assertFalse(key.exists())


class PreflightTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        shutil.copytree(publish.ROOT / "contrib/aur", self.source / "contrib/aur")
        (self.source / ".github").mkdir()
        shutil.copyfile(publish.ROOT / ".github/aur_known_hosts", self.source / ".github/aur_known_hosts")
        publish.run("git", "init", str(self.source))
        publish.run("git", "add", "contrib/aur", ".github", cwd=self.source)
        publish.run("git", "-c", "user.name=Test", "-c", "user.email=test@example.com",
                    "-c", "commit.gpgsign=false", "commit", "-m", "fixtures", cwd=self.source)
        publish.run("git", "-c", "tag.gpgsign=false", "tag", "v0.1.9", cwd=self.source)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        archive = "awob-0.1.9-x86_64-unknown-linux-gnu.tar.gz"
        (self.assets / archive).write_bytes(b"test archive")
        sha = hashlib.sha256(b"test archive").hexdigest()
        (self.assets / (archive + ".sha256")).write_text(sha + "  " + archive + "\n")
        for package, name in publish.PACKAGES.items():
            text = (self.source / "contrib/aur" / name).read_text()
            if package != "awob-git":
                text = text.replace("@VERSION@", "0.1.9").replace("@SHA256@", sha)
            (self.assets / f"awob-0.1.9.{package}.PKGBUILD").write_text(text)
        self.output = self.root / "ready"

    def prepare(self):
        publish.prepare("v0.1.9", self.assets, self.output, self.source)

    def test_every_real_package_generates_valid_metadata(self):
        self.prepare()
        self.assertEqual(len(list(self.output.glob("*/.SRCINFO"))), 9)

    def test_modified_archive_is_rejected(self):
        next(self.assets.glob("*.tar.gz")).write_bytes(b"bad archive")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.prepare()

    def test_modified_release_recipe_is_rejected_before_execution(self):
        (self.assets / "awob-0.1.9.awob-bin.PKGBUILD").write_text("exit 42\n")
        with self.assertRaisesRegex(ValueError, "tagged source"):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_missing_pin_is_rejected_before_preparation(self):
        (self.source / ".github/aur_known_hosts").unlink()
        with self.assertRaisesRegex(ValueError, "host key"):
            self.prepare()

    def test_only_stable_release_tags_are_accepted(self):
        for tag in ["main", "v0.1.9-rc1", "../main", "v0.1.9\n"]:
            with self.assertRaises(ValueError):
                publish.version_from_tag(tag)


if __name__ == "__main__":
    unittest.main()
