#!/usr/bin/env python3
"""Small offline fixtures; no Docker, Apple Container, network, or compilation."""
import gzip
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("oci", HERE / "container-oci.py")
oci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oci)


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.base = self.root / "base.tar"
        self.payload = self.root / "payload.tar"
        self.output = self.root / "final.tar"
        self.config = {"architecture": "arm64", "os": "linux",
                       "config": {"User": "997:997", "WorkingDir": "/app", "Entrypoint": ["./magic-supervisor"],
                                  "Env": ["MAGICIAN_ROOT_DIR=/data"], "Labels": {"org.opencontainers.image.revision": "old"}},
                       "rootfs": {"type": "layers", "diff_ids": []}, "history": []}
        self.write_base()
        self.write_payload()

    def write_base(self, corrupt=False, nested=False):
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w") as archive:
            oci.add_bytes(archive, "app/ui/obsolete.js", b"old UI")
            oci.add_bytes(archive, "app/scripts/obsolete.py", b"old script")
            oci.add_bytes(archive, "opt/dependency", b"keep")
        layer = gzip.compress(raw.getvalue(), mtime=0)
        desc = oci.blob(layer, oci.LAYER)
        self.config["rootfs"]["diff_ids"] = ["sha256:" + oci.hashlib.sha256(raw.getvalue()).hexdigest()]
        config = oci.encoded(self.config)
        cd = oci.blob(config, oci.CONFIG)
        manifest = oci.encoded(dict(schemaVersion=2, mediaType=oci.MANIFEST, config=cd, layers=[desc]))
        md = oci.blob(manifest, oci.MANIFEST)
        child = oci.encoded(dict(schemaVersion=2, mediaType=oci.INDEX, manifests=[md]))
        nd = oci.blob(child, oci.INDEX)
        with tarfile.open(self.base, "w") as archive:
            oci.add_bytes(archive, "oci-layout", oci.encoded({"imageLayoutVersion": "1.0.0"}))
            oci.add_bytes(archive, "index.json", oci.encoded(dict(schemaVersion=2, manifests=[nd if nested else md])))
            for d, content in ((cd, config), (md, manifest), (nd, child), (desc, layer)):
                if d == desc and corrupt:
                    content = b"!" + content[1:]
                oci.add_bytes(archive, "blobs/sha256/" + d["digest"].split(":")[1], content)

    def write_payload(self, arch="arm64", extra=None, bad_index=False):
        header = bytearray(64)
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = oci.ARCHES[arch].to_bytes(2, "little")
        with tarfile.open(self.payload, "w") as archive:
            for name in oci.BINARIES:
                oci.add_bytes(archive, "app/" + name, header, 0o755)
            if bad_index:
                member = tarfile.TarInfo("app/ui/index.html")
                member.type = tarfile.DIRTYPE
                archive.addfile(member)
            else:
                oci.add_bytes(archive, "app/ui/index.html", b"<html>new</html>")
            oci.add_bytes(archive, "app/scripts/current.py", b"#!/usr/bin/env python3\n", 0o755)
            oci.add_bytes(
                archive,
                "app/skillshub/browser/SKILL.md",
                b"---\nname: browser\ndescription: fixture\n---\n",
            )
            if extra:
                archive.addfile(extra, io.BytesIO(b""))

    def package(self):
        return oci.assemble(self.base, self.payload, self.output, "magician:fixture", "arm64")

    def test_package_preserves_runtime_and_replaces_complete_ui(self):
        record = self.package()
        image = oci.OCI(self.output)
        self.addCleanup(image.close)
        _, manifest, config = image.select("arm64")
        self.assertEqual(image.verify("arm64")["layers"], 2)
        for key in ("User", "WorkingDir", "Entrypoint", "Env"):
            self.assertEqual(config["config"][key], self.config["config"][key])
        self.assertNotIn("org.opencontainers.image.revision", config["config"]["Labels"])
        self.assertFalse(record["compilation_performed"])
        with image.read(image.blob_name(manifest["layers"][-1])) as source:
            raw = gzip.decompress(source.read())
        self.assertEqual("sha256:" + oci.hashlib.sha256(raw).hexdigest(), config["rootfs"]["diff_ids"][-1])
        with tarfile.open(fileobj=io.BytesIO(raw)) as layer:
            self.assertTrue(layer.getmember("app/ui/.wh..wh..opq").isfile())
            self.assertTrue(layer.getmember("app/scripts/.wh..wh..opq").isfile())
            self.assertEqual(layer.getmember("app/magician.bin").mode, 0o755)
            self.assertEqual(layer.getmember("app/scripts/current.py").mode, 0o755)

    def test_nested_apple_index(self):
        self.write_base(nested=True)
        self.package()

    def test_corrupt_base_refused_without_publishing(self):
        self.write_base(corrupt=True)
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            self.package()
        self.assertFalse(self.output.exists())

    def test_wrong_architecture_refused(self):
        self.write_payload(arch="amd64")
        with self.assertRaisesRegex(ValueError, "Linux arm64"):
            self.package()

    def test_path_and_file_type_rejections(self):
        for name, kind in (("app/ui/../../data/key", tarfile.REGTYPE), ("app/ui/.env", tarfile.REGTYPE),
                           ("app/ui/link", tarfile.SYMTYPE), ("app/ui/.wh.old", tarfile.REGTYPE),
                           ("app/ui/index.html", tarfile.REGTYPE), ("data/key", tarfile.REGTYPE)):
            with self.subTest(name=name):
                member = tarfile.TarInfo(name)
                member.type = kind
                member.linkname = "/data/key" if kind == tarfile.SYMTYPE else ""
                self.write_payload(extra=member)
                with self.assertRaises(ValueError):
                    oci.inspect_payload(self.payload, "arm64")

    def test_index_must_be_regular_html(self):
        self.write_payload(bad_index=True)
        with self.assertRaisesRegex(ValueError, "nonempty, regular"):
            self.package()

    def test_root_user_rejected(self):
        for user in ("", "root", "0:123", "root:magician"):
            with self.subTest(user=user):
                self.config["config"]["User"] = user
                self.write_base()
                with self.assertRaisesRegex(ValueError, "non-root"):
                    self.package()

    def test_modified_bundle_receipt_rejected(self):
        Path(str(self.payload) + ".json").write_text(json.dumps(dict(sha256="wrong")))
        with self.assertRaisesRegex(ValueError, "receipt digest mismatch"):
            self.package()

    def test_revision_follows_verified_artifact_receipt(self):
        Path(str(self.payload) + ".json").write_text(json.dumps(dict(sha256=oci.digest_file(self.payload), source_revision="snapshot-123")))
        self.assertEqual(self.package()["source_revision"], "snapshot-123")

    def test_bundle_from_separate_linux_outputs(self):
        binaries, ui, scripts = self.root / "release", self.root / "ui", self.root / "scripts"
        skills = self.root / "skillshub"
        binaries.mkdir()
        ui.mkdir()
        scripts.mkdir()
        (skills / "browser").mkdir(parents=True)
        with tarfile.open(self.payload) as archive:
            for name in oci.BINARIES:
                (binaries / name.removesuffix(".bin")).write_bytes(archive.extractfile("app/" + name).read())
        (ui / "index.html").write_text("<html>built</html>")
        runtime_script = scripts / "container-entrypoint.sh"
        runtime_script.write_text("#!/bin/sh\n")
        runtime_script.chmod(0o755)
        (skills / "browser" / "SKILL.md").write_text(
            "---\nname: browser\ndescription: fixture\n---\n")
        bundled = self.root / "bundled.tar"
        with contextlib.redirect_stdout(io.StringIO()):
            oci.main(["bundle", "--binaries", str(binaries), "--ui", str(ui),
                      "--scripts", str(scripts), "--skills", str(skills),
                      "--output", str(bundled),
                      "--arch", "arm64", "--revision", "reviewed-source"])
        self.payload = bundled
        self.assertEqual(self.package()["source_revision"], "reviewed-source")

    def test_existing_output_not_overwritten(self):
        self.output.write_bytes(b"keep")
        with self.assertRaises(FileExistsError):
            self.package()
        self.assertEqual(self.output.read_bytes(), b"keep")

    def test_build_helper_dry_run_and_active_tree_guard(self):
        source = self.root / "source"
        source.mkdir()
        for name in ("Cargo.lock", "Makefile"):
            (source / name).touch()
        def invoke(path, extra=()):
            return subprocess.run([sys.executable, str(HERE / "build-container-artifacts.py"), "--dry-run",
                                   "--source", str(path), "--cache", str(self.root / "cache"),
                                   "--output", str(self.output), "--engine", "apple", *extra], capture_output=True, text=True)
        result = invoke(source)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["cpus"], 2)
        self.assertEqual(json.loads(result.stdout)["cargo_jobs"], 1)
        self.assertFalse((self.root / "cache").exists())
        self.assertNotEqual(invoke(HERE.parent).returncode, 0)
        self.assertNotEqual(invoke(source, ("--jobs", "8")).returncode, 0)

        zig = subprocess.run([sys.executable, str(HERE / "build-container-artifacts.py"), "--dry-run",
                              "--source", str(source), "--cache", str(self.root / "zig-cache"),
                              "--output", str(self.output), "--engine", "zig", "--arch", "arm64"],
                             capture_output=True, text=True)
        self.assertEqual(zig.returncode, 0, zig.stderr)
        zig_plan = json.loads(zig.stdout)
        self.assertEqual(zig_plan["cargo_jobs"], 2)
        self.assertEqual(zig_plan["zig_target"], "aarch64-unknown-linux-gnu.2.36")
        self.assertIsNone(zig_plan["sdk_image"])
        self.assertIsNone(zig_plan["memory"])
        self.assertEqual(Path(zig_plan["compiler_tmp"]).parent.name, "tmp")
        self.assertRegex(Path(zig_plan["compiler_tmp"]).name, r"^zig-[0-9a-f]{12}$")
        self.assertFalse((self.root / "zig-cache").exists())
        custom_tmp = self.root / "short-tmp"
        custom_env = dict(os.environ, MAGICIAN_CONTAINER_BUILD_TMP_ROOT=str(custom_tmp))
        custom = subprocess.run(
            [sys.executable, str(HERE / "build-container-artifacts.py"), "--dry-run",
             "--source", str(source), "--cache", str(self.root / "custom-zig-cache"),
             "--output", str(self.output), "--engine", "zig"],
            capture_output=True, text=True, env=custom_env)
        self.assertEqual(custom.returncode, 0, custom.stderr)
        self.assertEqual(Path(json.loads(custom.stdout)["compiler_tmp"]).parent, custom_tmp.resolve())
        invalid_glibc = subprocess.run(
            [sys.executable, str(HERE / "build-container-artifacts.py"), "--dry-run",
             "--source", str(source), "--cache", str(self.root / "bad-zig-cache"),
             "--output", str(self.output), "--engine", "zig", "--glibc", "bookworm"],
            capture_output=True, text=True)
        self.assertNotEqual(invalid_glibc.returncode, 0)


if __name__ == "__main__":
    unittest.main()
