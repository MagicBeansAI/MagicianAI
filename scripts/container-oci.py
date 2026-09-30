#!/usr/bin/env python3
"""Local prebuilt-image pipeline. Packaging needs Python, not a running builder.

Export an OCI base once; capture or build Linux binaries/UI/runtime scripts; append that payload
to the base without compiling, installing dependencies, or reading runtime data.
"""
from __future__ import annotations

import argparse
import copy
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid

INDEX = "application/vnd.oci.image.index.v1+json"
MANIFEST = "application/vnd.oci.image.manifest.v1+json"
CONFIG = "application/vnd.oci.image.config.v1+json"
LAYER = "application/vnd.oci.image.layer.v1.tar+gzip"
BINARIES = ("magician.bin", "magicutor.bin", "magic-supervisor")
ARCHES = {"arm64": 183, "amd64": 62}
CHUNK = 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def digest_file(path):
    with Path(path).open("rb") as source:
        return digest_stream(source)


def digest_stream(source, destination=None):
    digest = hashlib.sha256()
    while chunk := source.read(CHUNK):
        digest.update(chunk)
        if destination is not None:
            destination.write(chunk)
    return "sha256:" + digest.hexdigest()


def reference(value):
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/:@-]*", value), "invalid image reference")
    return value


def command(argv, stdout=None):
    program = shutil.which(str(argv[0]))
    require(program, f"missing program: {argv[0]}")
    subprocess.run([program, *map(str, argv[1:])], check=True, stdout=stdout)


def output_slot(path):
    path = Path(path).expanduser().resolve()
    require(not path.exists(), f"output already exists: {path}")
    require(not path.with_suffix(path.suffix + ".json").exists(), "output receipt already exists")
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def receipt(path, data):
    record = dict(data, sha256=digest_file(path), bytes=path.stat().st_size)
    path.with_suffix(path.suffix + ".json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps(dict(record, output=str(path)), indent=2))
    return record


def publish(temp, output):
    # No overwrite even if another process creates the destination mid-build.
    os.link(temp, output)


class OCI:
    def __init__(self, path):
        self.tar = tarfile.open(path, "r:")
        self.members = {}
        for member in self.tar:
            name = member.name.removeprefix("./").rstrip("/")
            require(name not in self.members, f"duplicate archive entry: {name}")
            self.members[name] = member
        require(self.json_name("oci-layout").get("imageLayoutVersion") == "1.0.0", "not an OCI image layout")
        self.index = self.json_name("index.json")
        require(self.index.get("schemaVersion") == 2, "unsupported OCI index")

    def close(self):
        self.tar.close()

    def read(self, name):
        member = self.members.get(name)
        require(member is not None and member.isfile(), f"missing regular OCI entry: {name}")
        return self.tar.extractfile(member)

    def json_name(self, name):
        with self.read(name) as source:
            data = source.read(8 * CHUNK + 1)
        require(len(data) <= 8 * CHUNK, "OCI metadata exceeds 8 MiB")
        return json.loads(data)

    def blob_name(self, descriptor):
        digest = descriptor.get("digest", "")
        require(re.fullmatch(r"sha256:[a-f0-9]{64}", digest), "only SHA-256 OCI blobs are supported")
        name = "blobs/sha256/" + digest.split(":")[1]
        member = self.members.get(name)
        require(member is not None and member.isfile() and member.size == descriptor.get("size"),
                f"missing or incorrectly sized blob: {digest}")
        return name

    def json_blob(self, descriptor):
        name = self.blob_name(descriptor)
        with self.read(name) as source:
            data = source.read(8 * CHUNK + 1)
        require(len(data) <= 8 * CHUNK, "OCI metadata exceeds 8 MiB")
        require("sha256:" + hashlib.sha256(data).hexdigest() == descriptor["digest"], "OCI metadata digest mismatch")
        return json.loads(data)

    def select(self, arch):
        found = []
        def visit(descriptor, depth=0):
            require(depth < 8, "OCI index nesting exceeds limit")
            media = descriptor.get("mediaType")
            if media == INDEX:
                for child in self.json_blob(descriptor)["manifests"]:
                    visit(child, depth + 1)
            elif media == MANIFEST:
                manifest = self.json_blob(descriptor)
                config = self.json_blob(manifest["config"])
                if config.get("os") == "linux" and config.get("architecture") == arch:
                    require(config.get("rootfs", {}).get("type") == "layers", "invalid rootfs config")
                    require(len(config["rootfs"]["diff_ids"]) == len(manifest["layers"]), "layer/diff-ID count mismatch")
                    found.append((descriptor, manifest, config))
        for descriptor in self.index["manifests"]:
            visit(descriptor)
        require(len(found) == 1, f"expected exactly one Linux/{arch} image, found {len(found)}")
        return found[0]

    def verify(self, arch):
        descriptor, manifest, config = self.select(arch)
        for layer in manifest["layers"]:
            with self.read(self.blob_name(layer)) as source:
                require(digest_stream(source) == layer["digest"], "OCI layer digest mismatch")
        return {"schema": "magician.oci.v1", "platform": "linux/" + arch,
                "manifest_digest": descriptor["digest"], "layers": len(manifest["layers"]),
                "user": config.get("config", {}).get("User"), "blob_digests_verified": True}


def payload_name(member):
    name = member.name.removeprefix("./").rstrip("/")
    path = PurePosixPath(name)
    require(not path.is_absolute() and ".." not in path.parts, "unsafe artifact path")
    require(str(path) == name, "artifact path must be canonical")
    require(name in {"app/" + n for n in BINARIES}
            or name == "app/ui" or name.startswith("app/ui/")
            or name == "app/scripts" or name.startswith("app/scripts/")
            or (name.startswith("app/skillshub/") and name.endswith("/SKILL.md")),
            f"artifact outside binary/UI/runtime-script allowlist: {name}")
    require(member.isfile() or member.isdir(), f"artifact links/special files are not accepted: {name}")
    require(not any(p == ".env" or p.startswith(".env.") or p.startswith(".wh.") for p in path.parts),
            "env files and whiteouts are not accepted in artifacts")
    return name


def inspect_payload(path, arch):
    seen, headers, index, scripts, skills = set(), {}, False, False, False
    with tarfile.open(path, "r:") as archive:
        for member in archive:
            name = payload_name(member)
            require(name not in seen, f"duplicate artifact: {name}")
            seen.add(name)
            if name == "app/ui/index.html":
                index = member.isfile() and member.size > 0
            if name == "app/scripts" or name.startswith("app/scripts/"):
                scripts = True
            if name.startswith("app/skillshub/") and name.endswith("/SKILL.md"):
                skills = True
            if name in {"app/" + n for n in BINARIES}:
                require(member.isfile(), "service binary must be a regular file")
                with archive.extractfile(member) as source:
                    header = source.read(20)
                require(header[:6] == b"\x7fELF\x02\x01" and len(header) == 20
                        and int.from_bytes(header[18:20], "little") == ARCHES[arch],
                        f"{name} must be a Linux {arch} ELF binary; macOS binaries cannot be packaged")
                headers[name] = member.size
        require(len(headers) == len(BINARIES), "artifact bundle must include all three service binaries")
        require(index, "artifact bundle must include a nonempty, regular app/ui/index.html")
        require(scripts, "artifact bundle must include the tracked app/scripts directory")
        require(skills, "artifact bundle must include tracked Skillshub manifests")
    return {"platform": "linux/" + arch, "binaries": headers, "entries": len(seen),
            "runtime_scripts": True, "skill_manifests": True}


def add_bytes(archive, name, content, mode=0o644):
    member = tarfile.TarInfo(name)
    member.size, member.mode, member.mtime = len(content), mode, 0
    archive.addfile(member, io.BytesIO(content))


def blob(content, media):
    return {"mediaType": media, "digest": "sha256:" + hashlib.sha256(content).hexdigest(), "size": len(content)}


def assemble(base_path, artifacts, output, tag, arch, revision=None):
    inspect_payload(artifacts, arch)
    artifact_digest = digest_file(artifacts)
    record_path = Path(str(artifacts) + ".json")
    if record_path.exists():
        record = json.loads(record_path.read_text())
        require(record.get("sha256") == artifact_digest, "artifact receipt digest mismatch")
        revision = revision or record.get("source_revision")
    base = OCI(base_path)
    try:
        parent, manifest, config = base.select(arch)
        require(str(config.get("config", {}).get("User") or "").split(":")[0] not in ("", "root", "0"),
                "runtime base must configure a non-root user")
        with tempfile.TemporaryDirectory(prefix=".oci-package-", dir=output.parent) as scratch:
            scratch = Path(scratch)
            layer_tar, layer_gzip = scratch / "payload.tar", scratch / "payload.tar.gz"
            with tarfile.open(layer_tar, "w", format=tarfile.PAX_FORMAT) as dest:
                # A complete UI replaces the prior UI, including removed assets.
                add_bytes(dest, "app/ui/.wh..wh..opq", b"")
                # Scripts are sourced from the immutable committed snapshot, so
                # deletions must hide files that existed in the cached base.
                add_bytes(dest, "app/scripts/.wh..wh..opq", b"")
                with tarfile.open(artifacts, "r:") as source:
                    for original in source:
                        name = payload_name(original)
                        member = copy.copy(original)
                        member.name, member.uid, member.gid = name, 0, 0
                        member.uname = member.gname = "root"
                        member.mtime, member.pax_headers = 0, {}
                        member.mode = (0o755 if member.isdir()
                                       or name in {"app/" + n for n in BINARIES}
                                       or (name.startswith("app/scripts/") and original.mode & 0o111)
                                       else 0o644)
                        stream = source.extractfile(original) if original.isfile() else None
                        try:
                            dest.addfile(member, stream)
                        finally:
                            if stream:
                                stream.close()
            diff_id = digest_file(layer_tar)
            with layer_tar.open("rb") as source, layer_gzip.open("wb") as target:
                with gzip.GzipFile(fileobj=target, mode="wb", filename="", mtime=0, compresslevel=1) as compressed:
                    shutil.copyfileobj(source, compressed, CHUNK)
            layer = {"mediaType": LAYER, "digest": digest_file(layer_gzip), "size": layer_gzip.stat().st_size}
            config = copy.deepcopy(config)
            config["rootfs"]["diff_ids"].append(diff_id)
            config.setdefault("history", []).append({"created_by": "magician local OCI assembly (prebuilt binaries/UI)"})
            labels = config.setdefault("config", {}).get("Labels") or {}
            config["config"]["Labels"] = labels
            labels["org.opencontainers.image.base.digest"] = parent["digest"]
            labels["io.magician.artifacts.digest"] = artifact_digest
            labels.pop("org.opencontainers.image.revision", None)
            if revision:
                labels["org.opencontainers.image.revision"] = revision
            config_bytes = encoded(config)
            config_descriptor = blob(config_bytes, CONFIG)
            result_manifest = {"schemaVersion": 2, "mediaType": MANIFEST,
                               "config": config_descriptor, "layers": manifest["layers"] + [layer]}
            manifest_bytes = encoded(result_manifest)
            descriptor = blob(manifest_bytes, MANIFEST)
            descriptor["platform"] = {"os": "linux", "architecture": arch}
            descriptor["annotations"] = {"org.opencontainers.image.ref.name": tag,
                                         "io.containerd.image.name": tag,
                                         "com.apple.containerization.image.name": tag}
            archive_path = scratch / "image.oci.tar"
            with tarfile.open(archive_path, "w", format=tarfile.PAX_FORMAT) as dest:
                add_bytes(dest, "oci-layout", encoded({"imageLayoutVersion": "1.0.0"}))
                add_bytes(dest, "index.json", encoded({"schemaVersion": 2, "mediaType": INDEX, "manifests": [descriptor]}))
                copied = set()
                for old in manifest["layers"]:
                    name = base.blob_name(old)
                    if name in copied:
                        continue
                    # Validate while copying, keeping peak memory bounded.
                    with base.read(name) as source:
                        reader = HashReader(source)
                        member = tarfile.TarInfo(name)
                        member.size = old["size"]
                        dest.addfile(member, reader)
                        require(reader.digest() == old["digest"], "base OCI layer digest mismatch")
                    copied.add(name)
                for desc, content in ((config_descriptor, config_bytes), (descriptor, manifest_bytes)):
                    add_bytes(dest, "blobs/sha256/" + desc["digest"].split(":")[1], content)
                member = tarfile.TarInfo("blobs/sha256/" + layer["digest"].split(":")[1])
                member.size = layer["size"]
                with layer_gzip.open("rb") as source:
                    dest.addfile(member, source)
            publish(archive_path, output)
        return {"schema": "magician.oci.v1", "platform": "linux/" + arch, "tag": tag,
                "manifest_digest": descriptor["digest"], "base_manifest_digest": parent["digest"],
                "artifact_digest": artifact_digest, "source_revision": revision, "compilation_performed": False}
    finally:
        base.close()


class HashReader:
    def __init__(self, source):
        self.source, self.hasher = source, hashlib.sha256()

    def read(self, size=-1):
        data = self.source.read(size)
        self.hasher.update(data)
        return data

    def digest(self):
        return "sha256:" + self.hasher.hexdigest()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    for name in ("export", "capture", "bundle", "package", "verify"):
        sub = commands.add_parser(name)
        sub.add_argument("--arch", choices=ARCHES, default="arm64")
        if name != "verify":
            sub.add_argument("--output", type=Path, required=True)
        if name in ("export", "capture"):
            sub.add_argument("--image", type=reference, required=True)
        if name == "bundle":
            sub.add_argument("--binaries", type=Path, required=True)
            sub.add_argument("--ui", type=Path, required=True)
            sub.add_argument("--scripts", type=Path, required=True)
            sub.add_argument("--skills", type=Path, required=True)
        if name == "package":
            sub.add_argument("--base", type=Path, required=True)
            sub.add_argument("--artifacts", type=Path, required=True)
            sub.add_argument("--tag", type=reference, required=True)
        if name in ("package", "bundle"):
            sub.add_argument("--revision", help="source revision of these artifacts, not the current checkout")
        if name == "verify":
            sub.add_argument("archive", type=Path)
    args = parser.parse_args(argv)
    if args.action == "verify":
        image = OCI(args.archive)
        try:
            print(json.dumps(image.verify(args.arch), indent=2))
        finally:
            image.close()
        return
    output = output_slot(args.output)
    if args.action == "package":
        receipt(output, assemble(args.base, args.artifacts, output, args.tag, args.arch, args.revision))
        return
    with tempfile.TemporaryDirectory(prefix=".oci-stage-", dir=output.parent) as scratch:
        temp = Path(scratch) / "artifact.tar"
        if args.action == "export":
            command(["container", "image", "save", "--platform", "linux/" + args.arch, "--output", temp, args.image])
            image = OCI(temp)
            try:
                record = dict(image.verify(args.arch), source_image=args.image)
            finally:
                image.close()
        elif args.action == "capture":
            # Image filesystem only. No mounts, no service startup, no live /data.
            with temp.open("wb") as out:
                command(["container", "run", "--rm", "--name", "magician-artifacts-" + uuid.uuid4().hex[:10],
                         "--cpus", "2", "--memory", "512m", "--arch", args.arch, "--progress", "none",
                         "--entrypoint", "/bin/tar", args.image, "-C", "/", "-cf", "-",
                         *("app/" + n for n in BINARIES), "app/ui"], stdout=out)
            record = dict(inspect_payload(temp, args.arch), source_image=args.image)
        else:
            with tarfile.open(temp, "w", format=tarfile.PAX_FORMAT) as dest:
                for name in BINARIES:
                    source = args.binaries / name
                    if not source.is_file() and name.endswith(".bin"):
                        source = args.binaries / name.removesuffix(".bin")
                    require(source.is_file() and not source.is_symlink(), f"missing regular binary: {source}")
                    dest.add(source, arcname="app/" + name, recursive=False)
                require(args.ui.is_dir() and not args.ui.is_symlink(), "UI build directory missing")
                dest.add(args.ui, arcname="app/ui")
                require(args.scripts.is_dir() and not args.scripts.is_symlink(),
                        "committed runtime scripts directory missing")
                dest.add(args.scripts, arcname="app/scripts")
                require(args.skills.is_dir() and not args.skills.is_symlink(),
                        "committed Skillshub directory missing")
                manifests = sorted(args.skills.rglob("SKILL.md"))
                require(manifests, "committed Skillshub manifests missing")
                for source in manifests:
                    require(source.is_file() and not source.is_symlink(),
                            f"Skillshub manifest must be a regular file: {source}")
                    relative = source.relative_to(args.skills)
                    dest.add(source, arcname=str(PurePosixPath("app/skillshub") / relative),
                             recursive=False)
            record = dict(inspect_payload(temp, args.arch), source_revision=args.revision)
        publish(temp, output)
    receipt(output, record)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"container-oci: {error}", file=sys.stderr)
        raise SystemExit(1)
