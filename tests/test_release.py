"""Release gates and recovery tests; all files and API calls are isolated."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("release", Path(__file__).resolve().parents[1] / "scripts/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.archive = self.root / release.asset_name("0.2.0")
        self.archive.write_bytes(b"isolated archive fixture")
        self.checksum = self.archive.with_suffix(".zip.sha256")
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.checksum.write_text(f"{digest}  {self.archive.name}\n", encoding="utf-8")
        self.env = patch.dict(os.environ, {
            "GITHUB_EVENT_NAME": "push", "GITHUB_REF": "refs/heads/master",
            "BEFORE": "a" * 40, "GITHUB_SHA": "b" * 40,
            "GITHUB_OUTPUT": str(self.root / "outputs"), "GITHUB_REPOSITORY": "owner/repo",
        })
        self.env.start()
        self.addCleanup(self.env.stop)

    def test_semver(self):
        for value in ("0.1.0", "1.2.3-beta.1", "1.2.3+build.4", "1.2.3-0"):
            self.assertIsNotNone(release.SEMVER.fullmatch(value), value)
        for value in ("01.2.3", "1.2", "v1.2.3", "1.2.3-01", "1.2.3\n", "1.2.٣"):
            self.assertIsNone(release.SEMVER.fullmatch(value), value)

    def test_lock_version_must_match(self):
        (self.root / "Cargo.toml").write_text('[package]\nname="app"\nversion="0.2.0"\n')
        (self.root / "Cargo.lock").write_text('[[package]]\nname="app"\nversion="0.1.0"\n')
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            release.version(self.root)
        (self.root / "Cargo.lock").write_text('[[package]]\nname="app"\nversion="0.2.0"\n')
        self.assertEqual(release.version(self.root), "0.2.0")

    def detect(self, previous, current="0.2.0"):
        with patch.object(release, "version", return_value=current), patch.object(
            release, "run", return_value=f'[package]\nversion="{previous}"\n'
        ):
            release.detect()
        return (self.root / "outputs").read_text()

    def test_version_change_releases(self):
        self.assertIn("changed=true", self.detect("0.1.0"))

    def test_dependency_only_change_skips(self):
        self.assertIn("changed=false", self.detect("0.2.0"))

    def test_manual_dispatch_releases_current_version(self):
        os.environ["GITHUB_EVENT_NAME"] = "workflow_dispatch"
        self.assertIn("changed=true", self.detect("0.2.0"))

    def test_new_branch_needs_explicit_dispatch(self):
        os.environ["BEFORE"] = "0" * 40
        self.assertIn("changed=false", self.detect("0.1.0"))

    def test_manual_dispatch_rejects_other_branch(self):
        os.environ["GITHUB_EVENT_NAME"] = "workflow_dispatch"
        os.environ["GITHUB_REF"] = "refs/heads/feature"
        with self.assertRaisesRegex(ValueError, "master"):
            self.detect("0.1.0")

    def publish(self, api, uploader=None):
        with patch.object(release, "version", return_value="0.2.0"), patch.object(
            release, "api", side_effect=api
        ) as calls, patch.object(release.subprocess, "run", side_effect=uploader) as upload:
            release.publish(self.root)
        return calls, upload

    def ref(self):
        return {"object": {"type": "commit", "sha": "b" * 40}}

    def assets(self):
        return [{"name": p.name, "size": p.stat().st_size, "state": "uploaded"}
                for p in (self.archive, self.checksum)]

    def test_new_release_uploads_before_publish(self):
        calls, upload = self.publish([None, {}, None, {"id": 7}, self.assets(), {}])
        self.assertEqual(calls.call_args_list[1].args[0:2], ("git/refs", "POST"))
        self.assertTrue(calls.call_args_list[3].args[2]["draft"])
        self.assertFalse(calls.call_args_list[-1].args[2]["draft"])
        upload.assert_called_once()

    def test_retry_resumes_draft(self):
        calls, upload = self.publish([self.ref(), {"id": 7, "draft": True}, self.assets(), {}])
        self.assertFalse(any(c.args[1:2] == ("POST",) for c in calls.call_args_list))
        upload.assert_called_once()

    def test_existing_public_release_is_untouched(self):
        calls, upload = self.publish([self.ref(), {"id": 7, "draft": False}])
        self.assertEqual(calls.call_count, 2)
        upload.assert_not_called()

    def test_tag_collision_fails_without_upload(self):
        with self.assertRaisesRegex(ValueError, "another commit"):
            self.publish([{"object": {"type": "commit", "sha": "c" * 40}}])

    def test_bad_checksum_fails_before_api(self):
        self.archive.write_bytes(b"corrupted")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.publish([])

    def test_incomplete_assets_leave_draft(self):
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            self.publish([self.ref(), {"id": 7, "draft": True}, []])

    def test_failed_upload_leaves_draft(self):
        with self.assertRaises(release.subprocess.CalledProcessError):
            self.publish([self.ref(), {"id": 7, "draft": True}],
                         release.subprocess.CalledProcessError(1, "gh"))

    def test_prerelease_does_not_replace_latest(self):
        value = "0.2.0-beta.1"
        archive = self.root / release.asset_name(value)
        archive.write_bytes(self.archive.read_bytes())
        checksum = archive.with_suffix(".zip.sha256")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        checksum.write_text(f"{digest}  {archive.name}\n")
        assets = [{"name": p.name, "size": p.stat().st_size, "state": "uploaded"}
                  for p in (archive, checksum)]
        with patch.object(release, "version", return_value=value), patch.object(
            release, "api", side_effect=[self.ref(), {"id": 7, "draft": True}, assets, {}]
        ) as api, patch.object(release.subprocess, "run"):
            release.publish(self.root)
        self.assertEqual(api.call_args.args[2], {
            "draft": False, "prerelease": True, "make_latest": "false"})

    def test_package_contains_runtime_notices_and_valid_checksum(self):
        (self.root / "docs").mkdir()
        (self.root / "assets/web/vendor").mkdir(parents=True)
        for name in ("README.md", "README_zh.md", "docs/demo.md", "assets/web/vendor/example-LICENSE.txt"):
            (self.root / name).write_text("fixture")
        # Registry archives can carry Unix-epoch timestamps, outside ZIP's range.
        os.utime(self.root / "assets/web/vendor/example-LICENSE.txt", (0, 0))
        binary = self.root / "app.exe"
        binary.write_bytes(b"MZ")
        runtime = self.root / "runtime"
        runtime.mkdir()
        (runtime / "vcruntime140.dll").write_bytes(b"runtime fixture")
        with patch.object(release, "ROOT", self.root), patch.object(
            release, "version", return_value="0.2.0"
        ), patch.object(release, "run", return_value=json.dumps({
            "packages": [], "workspace_members": [], "resolve": {"nodes": []}
        })):
            release.package(binary, runtime, self.root / "out")
        archive = self.root / "out" / release.asset_name("0.2.0")
        with zipfile.ZipFile(archive) as bundle:
            self.assertIsNone(bundle.testzip())
            self.assertTrue({"native-markdown.exe", "vcruntime140.dll", "START-HERE.txt",
                             "licenses/web/example-LICENSE.txt", "licenses/rust-packages.json"}
                            .issubset(bundle.namelist()))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual(archive.with_suffix(".zip.sha256").read_text(), f"{digest}  {archive.name}\n")


if __name__ == "__main__":
    unittest.main()
