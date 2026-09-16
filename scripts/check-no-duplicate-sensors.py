#!/usr/bin/env python3
"""Fail if a new sensor file is added without the canonical shape, or if a
known sensor file gains a duplicated walker / helper.

Run: python3 scripts/check-no-duplicate-sensors.py

The audit at `docs/CONTRIBUTING_AGENTS.md` documents why each sensor
file's walker, `to_snake_case`, `to_camel_case`, and handler-lookup
should live in one place (`sensors/util.rs` and the `Sensor` trait
default impls) rather than being reimplemented per sensor. This
check enforces the future invariant:

  - No sensor file may contain TWO copies of the
    `ignore::WalkBuilder::new(root).hidden(true).git_ignore(true)`
    walker shell. After Phase 3.1 (walker extraction), every sensor
    has zero such shells — the shared walker lives in
    `sensors/util.rs` and the per-file loops call
    `util::walk_workspace(root)`. A new file that re-introduces one
    in-line is a copy-paste drift.

  - Every sensor file must follow the trait shape: a unit struct +
    `inventory::submit!(SensorEntry(&…))`. There is no legacy
    exemption; the Phase 3.1 migration is complete.

Exit codes:
  0  — no new violations
  1  — at least one violation outside the known baseline
"""
from __future__ import annotations

import argparse
import os
import re
import sys

DEFAULT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SENSORS_DIRNAME = os.path.join("src", "server", "sensors")

# Phase 3.1 (walker extraction) is complete: every sensor file under
# `sensors/` follows the trait shape — `pub struct XxxSensor;`,
# `impl Sensor for XxxSensor`, and `inventory::submit!(SensorEntry(&…))`.
# The check below enforces that uniformly, with no legacy exemption.

WALKER_PATTERN = re.compile(
    r"ignore::WalkBuilder::new\(root\)\s*\.\s*hidden\(true\)",
)
INVENTORY_SUBMIT_PATTERN = re.compile(
    r"inventory::submit!\s*\(\s*SensorEntry\s*\(\s*&\s*([A-Za-z_][A-Za-z_0-9]*)",
)
UNIT_STRUCT_PATTERN = re.compile(
    r"^\s*pub\s+struct\s+([A-Za-z_][A-Za-z_0-9]*)\s*;\s*$",
    re.MULTILINE,
)


def sensor_basenames(root: str) -> list[str]:
    sensors_dir = os.path.join(root, SENSORS_DIRNAME)
    out: list[str] = []
    if not os.path.isdir(sensors_dir):
        return out
    for name in os.listdir(sensors_dir):
        if name.endswith("_sensor.rs") and os.path.isfile(
            os.path.join(sensors_dir, name)
        ):
            out.append(name[: -len(".rs")])
    return sorted(out)


def violations(root: str) -> list[str]:
    out: list[str] = []
    basenames = sensor_basenames(root)
    for base in basenames:
        path = os.path.join(root, SENSORS_DIRNAME, base + ".rs")
        with open(path, encoding="utf-8") as f:
            text = f.read()
        walker_hits = WALKER_PATTERN.findall(text)
        if len(walker_hits) >= 2:
            out.append(
                f"{path}: walker shell duplicated {len(walker_hits)}× "
                f"(expected at most 1; the shared walker lives in "
                f"sensors/util.rs or the Sensor trait)"
            )
        submits = INVENTORY_SUBMIT_PATTERN.findall(text)
        if not submits:
            out.append(
                f"{path}: new sensor file is missing "
                f"`inventory::submit!(SensorEntry(&XxxSensor))`. "
                f"See src/server/sensors/AGENTS.md and "
                f"docs/CONTRIBUTING_AGENTS.md#sensor-pattern."
            )
            continue
        struct_names = {m for m in UNIT_STRUCT_PATTERN.findall(text)}
        if not submits[0] in struct_names:
            out.append(
                f"{path}: `inventory::submit!(SensorEntry(&{submits[0]}))` "
                f"does not match any unit struct declared in the same file."
            )
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--root",
        default=DEFAULT_ROOT,
        help="Project root (default: %(default)s)",
    )
    parsed = parser.parse_args()
    root = os.path.abspath(parsed.root)
    sensors_dir = os.path.join(root, SENSORS_DIRNAME)
    if not os.path.isdir(sensors_dir):
        print(f"sensors dir not found: {sensors_dir}", file=sys.stderr)
        return 2
    v = violations(root)
    if v:
        for line in v:
            print(line, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())