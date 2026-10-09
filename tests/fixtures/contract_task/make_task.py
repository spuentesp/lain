#!/usr/bin/env python3
"""Build the cross-repo contract-task fixture (test 3).

Base = `scripts/contracts-fixture.sh` (deterministic 4-repo fixture:
orders Rust/axum + openapi.yaml, billing Python/FastAPI, reports TS,
platform monorepo). On top of it this script drops the task's oracle
tests — red before the contract change, green after — and commits them
so the change itself is a clean, reviewable commit.

The change (see ground_truth.json): orders renames the RESPONSE field
`customer_id` -> `customerId` (request model keeps its name); billing's
reads must follow. The fixture already carries the provider side as the
scenario tag `s11-rename-field`, so `apply-edit` materializes exactly
that content instead of duplicating it.

Usage:
    make_task.py build <dir>        # fixture + oracle tests + setup commit
    make_task.py apply-edit <dir>   # scripted change (graph mode only)
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(HERE)))
FIXTURE_SH = os.path.join(REPO_ROOT, "scripts", "contracts-fixture.sh")

BILLING_TEST = '''\
"""Behavioral oracle for the contract change: with orders' NEW response
field name, build_invoice must still produce a complete invoice.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

os.environ.setdefault("ORDERS_URL", "http://orders.invalid")


def _stub_module(name, **attrs):
    mod = types.ModuleType(name)
    for k, v in attrs.items():
        setattr(mod, k, v)
    sys.modules.setdefault(name, mod)
    return sys.modules[name]


class _BaseModel:
    def __init__(self, **kwargs):
        for k, v in kwargs.items():
            setattr(self, k, v)


class _FastAPI:
    def get(self, _path):
        def deco(fn):
            return fn
        return deco


httpx_stub = _stub_module("httpx")
_stub_module("fastapi", FastAPI=_FastAPI)
_stub_module("pydantic", BaseModel=_BaseModel)

sys.path.insert(0, os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src"))
import main  # noqa: E402  (billing's service module)


class _Resp:
    text = ""

    def __init__(self, payload):
        self._payload = payload

    def json(self):
        return self._payload


class BuildInvoiceReadsTheRenamedField(unittest.TestCase):
    def test_reads_the_new_field_name(self):
        # orders' response after the rename — billing must follow it.
        httpx_stub.get = lambda url: _Resp({"customerId": "cust-1", "total": 5})
        main.httpx = httpx_stub
        invoice = main.build_invoice("ord-1")
        self.assertEqual(invoice.customer_id, "cust-1")
        self.assertEqual(invoice.total, 5)


if __name__ == "__main__":
    unittest.main()
'''

ORDERS_TEST = '''\
"""Contract-artifact oracle for the change: orders' RESPONSE schema must
use the wire field name `customerId`. The request model (CreateOrder) is
out of scope — it keeps `customer_id` on purpose.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _read(rel):
    with open(os.path.join(ROOT, rel)) as f:
        return f.read()


def _block(text, start, end):
    return text.split(start, 1)[1].split(end, 1)[0]


class ResponseFieldIsCustomerId(unittest.TestCase):
    def test_order_struct_declares_wire_name(self):
        text = _read("src/orders/models.rs")
        order = _block(text, "pub struct Order", "pub struct CreateOrder")
        self.assertIn("customerId", order)

    def test_openapi_order_schema_uses_new_name(self):
        text = _read("openapi.yaml")
        order = _block(text, "    Order:", "    CreateOrder:")
        self.assertIn("customerId", order)
        self.assertNotIn("customer_id", order)


if __name__ == "__main__":
    unittest.main()
'''


def _git(dir_, *args):
    env = dict(os.environ)
    env.update(GIT_AUTHOR_NAME="contract-task-fixture", GIT_AUTHOR_EMAIL="fixture@lain.local",
               GIT_COMMITTER_NAME="contract-task-fixture", GIT_COMMITTER_EMAIL="fixture@lain.local")
    subprocess.run(["git", "-C", dir_, *args], check=True, env=env, capture_output=True)


def build(dest):
    subprocess.run(["bash", FIXTURE_SH, dest], check=True, capture_output=True)
    for rel, body in (("billing/tests/test_invoice_contract.py", BILLING_TEST),
                      ("orders/tests/test_contract_artifact.py", ORDERS_TEST)):
        path = os.path.join(dest, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(body)
    for repo in ("orders", "billing"):
        _git(os.path.join(dest, repo), "add", "-A")
        _git(os.path.join(dest, repo), "commit", "-q", "-m",
             "tests: contract-task oracle (red until the rename lands)")
    return dest


def apply_edit(dest):
    """The scripted change: materialize the fixture's own `s11-rename-field`
    content on the provider side (no duplication of what the fixture already
    defines), and follow the rename on the consumer side."""
    orders = os.path.join(dest, "orders")
    _git(orders, "checkout", "s11-rename-field", "--", ".")
    main_py = os.path.join(dest, "billing", "src", "main.py")
    with open(main_py) as f:
        body = f.read()
    fixed = body.replace('order["customer_id"]', 'order["customerId"]')
    if fixed == body:
        raise SystemExit("apply-edit: expected the customer_id read in billing/src/main.py")
    with open(main_py, "w") as f:
        f.write(fixed)
    return dest


def main(argv):
    if len(argv) != 3 or argv[1] not in ("build", "apply-edit"):
        print(__doc__, file=sys.stderr)
        return 2
    cmd, dest = argv[1], argv[2]
    if cmd == "build":
        if os.path.isdir(dest) and os.listdir(dest):
            print(f"refusing to build into non-empty directory: {dest}", file=sys.stderr)
            return 1
        os.makedirs(dest, exist_ok=True)
        build(dest)
    else:
        apply_edit(dest)
    print(dest)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
