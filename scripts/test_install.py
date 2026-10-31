"""Offline behavioral checks for the native release installer."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"
VERSION = "v1.2.3"
BINARY = b"#!/bin/sh\nprintf 'installed fixture\\n'\n"
TARGETS = {
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
}


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.assets = self.root / "assets"
        self.assets.mkdir()
        checksums = []
        for target in TARGETS.values():
            name = f"carry-{VERSION}-{target}.tar.gz"
            archive = self.assets / name
            with tarfile.open(archive, "w:gz") as tar:
                info = tarfile.TarInfo("carry")
                info.mode = 0o755
                info.size = len(BINARY)
                tar.addfile(info, io.BytesIO(BINARY))
            checksums.append(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {name}\n")
        (self.assets / "SHA256SUMS").write_text("".join(checksums))
        self.bin = self.root / "mock-bin"
        self.bin.mkdir()
        self.mock("uname", """import os,sys
print(os.environ['TEST_OS'] if sys.argv[-1] == '-s' else os.environ['TEST_ARCH'])
""")
        self.mock("curl", """import json,os,pathlib,shutil,sys
args=sys.argv[1:]
root=pathlib.Path(os.environ['TEST_ROOT'])
with (root/'downloads.jsonl').open('a') as log: log.write(json.dumps(args)+'\\n')
if '-w' in args or '--write-out' in args:
    print('https://github.com/wiggzz/carry/releases/tag/v1.2.3', end='')
else:
    flag='-o' if '-o' in args else '--output'
    source=root/'assets'/args[-1].rsplit('/',1)[-1]
    if not source.exists(): sys.exit(22)
    shutil.copyfile(source,args[args.index(flag)+1])
""")
        self.destination = self.root / "custom bin with spaces"
        self.home = self.root / "home"
        self.home.mkdir()
        self.scratch = self.root / "scratch"
        self.scratch.mkdir()

    def mock(self, name, body):
        command = self.bin / name
        command.write_text(f"#!{sys.executable}\n{body}")
        command.chmod(0o755)

    def run_installer(self, system="Darwin", architecture="arm64", custom=True):
        env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                   HOME=str(self.home), TMPDIR=str(self.scratch), TEST_ROOT=str(self.root),
                   TEST_OS=system, TEST_ARCH=architecture)
        if custom:
            env["CARRY_INSTALL_DIR"] = str(self.destination)
        else:
            env.pop("CARRY_INSTALL_DIR", None)
        return subprocess.run(["sh", str(INSTALLER)], env=env, text=True,
                              capture_output=True)

    def downloads(self):
        path = self.root / "downloads.jsonl"
        return [json.loads(line)[-1] for line in path.read_text().splitlines()] if path.exists() else []

    def assert_success(self, result, destination):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((destination / "carry").is_file(), "installer must install the native release binary")
        self.assertEqual((destination / "carry").read_bytes(), BINARY)
        self.assertTrue(os.access(destination / "carry", os.X_OK))
        self.assertEqual(list(self.scratch.iterdir()), [], "temporary downloads must be cleaned up")

    def test_installs_matching_binary_for_each_supported_platform(self):
        for (system, architecture), target in TARGETS.items():
            with self.subTest(system=system, architecture=architecture):
                result = self.run_installer(system, architecture)
                self.assert_success(result, self.destination)
                self.assertIn(f"https://github.com/wiggzz/carry/releases/download/{VERSION}/carry-{VERSION}-{target}.tar.gz", self.downloads())

    def test_defaults_to_user_local_bin_and_prints_path_hint(self):
        result = self.run_installer(custom=False)
        self.assert_success(result, self.home / ".local" / "bin")
        self.assertIn(str(self.home / ".local" / "bin"), result.stdout)
        self.assertIn("PATH", result.stdout)

    def test_checksum_mismatch_preserves_existing_installation(self):
        sums = self.assets / "SHA256SUMS"
        lines = sums.read_text().splitlines()
        name = f"carry-{VERSION}-aarch64-apple-darwin.tar.gz"
        sums.write_text("\n".join(("0" * 64 + "  " + name) if line.endswith(name)
                                  else line for line in lines) + "\n")
        self.destination.mkdir()
        existing = self.destination / "carry"
        existing.write_text("existing installation")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, "mismatched release checksum must stop installation")
        self.assertEqual(existing.read_text(), "existing installation")
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_missing_checksum_stops_before_install(self):
        (self.assets / "SHA256SUMS").write_text("")
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_failed_download_does_not_install(self):
        (self.assets / f"carry-{VERSION}-aarch64-apple-darwin.tar.gz").unlink()
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.destination.exists())
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_unsupported_platform_fails_without_downloads(self):
        for system, architecture in (("Linux", "aarch64"), ("Windows_NT", "x86_64")):
            with self.subTest(system=system, architecture=architecture):
                result = self.run_installer(system, architecture)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.downloads(), [])
                self.assertFalse(self.destination.exists())


if __name__ == "__main__":
    unittest.main()
