#!/usr/bin/env python3
"""Compile the firmware's connection logic with ESP-IDF's actual JSON parser."""
import os
from pathlib import Path
import subprocess

repo = Path(__file__).resolve().parents[1]
idf = Path(os.environ.get("IDF_PATH", str(Path.home() / "esp/esp-idf")))
cjson = idf / "components/json/cJSON"
if not (cjson / "cJSON.c").is_file():
    raise SystemExit("Set IDF_PATH to the installed ESP-IDF checkout (cJSON source required).")
build = Path(os.environ.get("MAGESP_TEST_DIR", str(Path(os.environ.get("CARGO_TARGET_DIR", str(repo / "target")))) + "/magesp-connection-tests"))
build.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, TMPDIR=str(build))
binary = build / "connection-test"
parser = build / "cJSON.o"
subprocess.run([
    os.environ.get("CC", "cc"), "-std=c11", "-Wno-deprecated-declarations",
    "-fsanitize=address,undefined", "-g", "-I", str(cjson),
    "-c", str(cjson / "cJSON.c"), "-o", str(parser),
], env=env, check=True)
subprocess.run([
    os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
    "-fsanitize=address,undefined", "-g", "-I", str(repo / "magesp/main"),
    "-I", str(cjson), str(repo / "magesp/main/connection.c"),
    str(repo / "magesp/tests/connection_test.c"), str(parser),
    "-o", str(binary),
], env=env, check=True)
subprocess.run([str(binary)], env=env, check=True)
