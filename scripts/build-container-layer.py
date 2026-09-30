#!/usr/bin/env python3
"""Build a cached image layer, preserving an idle Apple builder's resources."""
import argparse
import importlib.util
from pathlib import Path
import shlex
import subprocess

source = Path(__file__).with_name("prepare-container-image.py")
spec = importlib.util.spec_from_file_location("prepare_container_image", source)
prepare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare)


def run(argv, cwd=None, capture=False):
    argv = list(map(str, argv))
    print("+ " + shlex.join(argv), flush=True)
    return subprocess.run(argv, cwd=cwd, text=True, capture_output=capture, check=not capture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", choices=("container", "docker"), default="container")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    arguments = args.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    if args.engine == "docker":
        run(["docker", "build", *arguments])
        return
    # These flags can recreate Apple's shared builder. Provision changes
    # separately; this wrapper only repeats the inspected configuration.
    forbidden = {"--cpus", "-c", "--memory", "-m", "--dns", "--dns-domain", "--dns-option", "--dns-search"}
    if any(value.split("=", 1)[0] in forbidden for value in arguments):
        parser.error("Apple builder resource/DNS overrides are not allowed in a cached layer build")
    with prepare.image_builder(run) as limits:
        run(["container", "build", *limits, *arguments])


if __name__ == "__main__":
    main()
