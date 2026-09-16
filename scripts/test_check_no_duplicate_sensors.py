#!/usr/bin/env python3
"""Self-test for check-no-duplicate-sensors.py.

Exercises the four paths the check can take:
  1. Clean state (current sensors/) exits 0.
  2. Adding a 6th sensor without inventory::submit! exits 1.
  3. Adding a 6th sensor WITH inventory::submit! + unit struct exits 0.
  4. A file that contains the walker shell twice exits 1.

Run: python3 scripts/test_check_no_duplicate_sensors.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(ROOT, "scripts", "check-no-duplicate-sensors.py")
SENSORS_DIR = os.path.join(ROOT, "src", "server", "sensors")


def run(tmp: str) -> int:
    return subprocess.run(
        ["python3", SCRIPT, "--root", tmp],
        cwd=tmp,
        capture_output=True,
        text=True,
    ).returncode


def assert_eq(label: str, got: int, want: int) -> None:
    if got != want:
        print(f"FAIL {label}: exit {got}, want {want}", file=sys.stderr)
        sys.exit(1)
    print(f"OK   {label}")


def main() -> int:
    if not os.path.isfile(SCRIPT):
        print(f"missing script: {SCRIPT}", file=sys.stderr)
        return 2
    with tempfile.TemporaryDirectory() as raw:
        os.makedirs(os.path.join(raw, "src", "server", "sensors"))
        shutil.copytree(
            SENSORS_DIR,
            os.path.join(raw, "src", "server", "sensors"),
            dirs_exist_ok=True,
        )
        assert_eq("clean state", run(raw), 0)
        bad = os.path.join(raw, "src", "server", "sensors", "thrift_sensor.rs")
        with open(bad, "w", encoding="utf-8") as f:
            f.write("// new sensor without inventory::submit\n")
        assert_eq("new file, no submit", run(raw), 1)
        os.remove(bad)
        good = os.path.join(raw, "src", "server", "sensors", "thrift_sensor.rs")
        with open(good, "w", encoding="utf-8") as f:
            f.write(
                "pub struct ThriftSensor;\n"
                "impl crate::server::sensors::mod_::Sensor for ThriftSensor {}\n"
                "inventory::submit!(SensorEntry(&ThriftSensor));\n"
            )
        assert_eq("new file, with submit + struct", run(raw), 0)
        os.remove(good)
        dup = os.path.join(raw, "src", "server", "sensors", "dupwalker_sensor.rs")
        with open(dup, "w", encoding="utf-8") as f:
            f.write(
                "pub struct DupSensor;\n"
                "inventory::submit!(SensorEntry(&DupSensor));\n"
                "fn a() { let _ = ignore::WalkBuilder::new(root).hidden(true); }\n"
                "fn b() { let _ = ignore::WalkBuilder::new(root).hidden(true); }\n"
            )
        assert_eq("duplicate walker in one file", run(raw), 1)
    print("all tests passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())