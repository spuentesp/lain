#!/usr/bin/env python3
"""Build the swarm/deep-chain fixture — the deep-hop decisive experiment.

Seven deterministic git repos around ONE contract change: core renames
the RESPONSE field `customer_id` -> `customer_ref` (same type). The
oracle (ground_truth.json, declared BEFORE any query) is about harm:
- api (TS BFF) forwards the field                      -> must follow
- cli (TS) reads resp.customer_id                      -> must follow
- billing (Python) reads it through api's BFF API      -> must follow
- notify (Python) reads it dynamically, 3 hops out     -> must be DECLARED
  needs-investigation (dynamic key; grep proves nothing), never dropped
- partner-portal (external, declared in core's openapi) -> known-unknown
- reporting (Python) has its own build_invoice         -> HOMONYM TRAP
- catalog (Python) reads `total` from the same core    -> NEAR-MISS TRAP
  endpoint, not the renamed field

Oracle tests (stdlib-only unittest; frameworks stubbed, contract
artifacts read as text where the language has no runtime here) live in
all seven repos — the five chain repos red before `apply-edit`, green
after; the two traps green throughout.

Usage:
    make_task.py build <dir>        # 7 repos + oracle tests + commits
    make_task.py apply-edit <dir>   # the scripted change (graph mode)
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

# Deterministic commit identity/dates (same base epoch as
# scripts/contracts-fixture.sh): refs and tree bytes are stable.
BASE_EPOCH = 1736140800  # 2025-01-06T09:00:00Z

CORE_TEST = '''\
"""Contract-artifact oracle for the change: core's RESPONSE schema must
use the wire field name `customer_ref` (code: `#[serde(rename)]`, spec:
openapi Order schema).

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _read(rel):
    with open(os.path.join(ROOT, rel)) as f:
        return f.read()


def _block(text, start, end=None):
    body = text.split(start, 1)[1]
    return body if end is None else body.split(end, 1)[0]


class ResponseFieldIsCustomerRef(unittest.TestCase):
    def test_order_struct_declares_new_wire_name(self):
        text = _read("src/models.rs")
        self.assertIn(
            \'#[serde(rename = "customer_ref")]\\n    pub customer_id: String,\',
            text)

    def test_openapi_order_schema_uses_new_name(self):
        text = _read("openapi.yaml")
        order = _block(text, "    Order:")
        self.assertIn("required: [customer_ref, total, status]", order)
        self.assertIn("\\n        customer_ref:\\n", order)


if __name__ == "__main__":
    unittest.main()
'''

API_TEST = '''\
"""Contract-artifact oracle for the change: the BFF must read core's
NEW response field name and forward it under that same wire name.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _read(rel):
    with open(os.path.join(ROOT, rel)) as f:
        return f.read()


class ForwardOrderFollowsTheRenamedField(unittest.TestCase):
    def test_reads_and_forwards_the_new_wire_name(self):
        text = _read("src/forward.ts")
        self.assertIn("customer_ref", text)
        self.assertNotIn("customer_id", text)


if __name__ == "__main__":
    unittest.main()
'''

CLI_TEST = '''\
"""Contract-artifact oracle for the change: the CLI must read core's
NEW response field name for display.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class ShowOrderReadsTheRenamedField(unittest.TestCase):
    def test_reads_the_new_wire_name(self):
        with open(os.path.join(ROOT, "src", "show.ts")) as f:
            text = f.read()
        self.assertIn("resp.customer_ref", text)


if __name__ == "__main__":
    unittest.main()
'''

BILLING_TEST = '''\
"""Behavioral oracle for the change: with api's NEW forwarded field
name (core renamed it), build_invoice must re-emit the customer value
under that same new name. This is the DEEP-CHAIN middle hop — its
evidence chain runs billing -> api -> core.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("httpx", types.ModuleType("httpx"))

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import main  # noqa: E402  (billing's service module)


class BuildInvoiceEmitsTheForwardedName(unittest.TestCase):
    def test_emits_the_new_wire_name(self):
        # api's response after the rename — the field core renamed is
        # forwarded under its new wire name; billing must follow it.
        main.fetch_order = lambda order_id: {"customer_ref": "C-1", "total": 10}
        invoice = main.build_invoice("ord-1")
        self.assertEqual(invoice.get("customer_ref"), "C-1")
        self.assertEqual(invoice.get("amount"), 10)


if __name__ == "__main__":
    unittest.main()
'''

NOTIFY_TEST = '''\
"""Behavioral oracle for the change: with billing's NEW invoice field
name (core renamed it), send_notice must still put the customer value
in the body. This is the DEEP hop — the field is read through a dynamic
key (`customer_' + SUFFIX`), three HTTP hops from core; grep in this
repo cannot prove or disprove impact.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("httpx", types.ModuleType("httpx"))

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import notify  # noqa: E402  (notify's sender module)


class SendNoticeCarriesTheCustomerValue(unittest.TestCase):
    def test_body_carries_the_value(self):
        # billing's invoice after the rename — the value arrives under
        # the new wire name; the dynamic key must be updated to match.
        notify.fetch_invoice = lambda invoice_id: {"customer_ref": "C-1"}
        msg = notify.send_notice("inv-1")
        self.assertIn("C-1", msg["body"])


if __name__ == "__main__":
    unittest.main()
'''

REPORTING_TEST = '''\
"""Standalone oracle: reporting's build_invoice is built from its own
arguments (CSV report) and never touches a chain endpoint. This is the
HOMONYM TRAP — these tests are GREEN before AND after the change;
editing this repo is a clobbering / false positive.

These tests must PASS at all times. Do not edit them.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
from report_invoice import build_invoice  # noqa: E402


class ReportingBuildsFromItsOwnArgs(unittest.TestCase):
    def test_builds_an_invoice_from_arguments(self):
        inv = build_invoice("o1", "n", 5)
        self.assertEqual(inv["invoice_id"], "manual-o1")
        self.assertEqual(inv["customer"], "n")
        self.assertEqual(inv["total"], 5)
        self.assertEqual(inv["source"], "csv")


if __name__ == "__main__":
    unittest.main()
'''

CATALOG_TEST = '''\
"""Standalone oracle: catalog's summarize reads only `total` from
core's endpoint. This is the NEAR-MISS TRAP — same endpoint, different
field; these tests are GREEN before AND after the change; editing this
repo is a false positive.

These tests must PASS at all times. Do not edit them.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import catalog  # noqa: E402


class CatalogReadsOnlyTotal(unittest.TestCase):
    def test_summarize_returns_total_without_the_customer_field(self):
        catalog.fetch_order = lambda order_id: {"customer_id": "C-1", "total": 10}
        result = catalog.summarize("o1")
        self.assertEqual(result.get("total"), 10)
        self.assertNotIn("customer_id", result)
        self.assertNotIn("customer_ref", result)


if __name__ == "__main__":
    unittest.main()
'''


def _git(dir_, *args, offset_days=0):
    env = dict(os.environ)
    stamp = f"{BASE_EPOCH + offset_days * 86400} +0000"
    env.update(
        GIT_AUTHOR_NAME="swarm-deepchain-fixture", GIT_AUTHOR_EMAIL="fixture@lain.local",
        GIT_COMMITTER_NAME="swarm-deepchain-fixture", GIT_COMMITTER_EMAIL="fixture@lain.local",
        GIT_AUTHOR_DATE=stamp, GIT_COMMITTER_DATE=stamp)
    subprocess.run(["git", "-C", dir_, *args], check=True, env=env, capture_output=True)


def _write(path, body):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(body)


_GITIGNORE = """\
target/
Cargo.lock
__pycache__/
*.pyc
node_modules/
dist/
build/
*.egg-info/
.pytest_cache/
"""


def _init_repo(path):
    os.makedirs(path, exist_ok=True)
    with open(os.path.join(path, ".gitignore"), "w") as f:
        f.write(_GITIGNORE)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True,
                   capture_output=True)
    return path


def _commit_all(dir_, msg, offset_days=0):
    _git(dir_, "add", "-A")
    _git(dir_, "commit", "-q", "-m", msg, offset_days=offset_days)


# ── per-repo sources ────────────────────────────────────────────────────────

def _write_core(root):
    d = _init_repo(os.path.join(root, "core"))
    _write(os.path.join(d, "Cargo.toml"), '''\
[package]
name = "core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
''')
    _write(os.path.join(d, "src", "main.rs"), '''\
mod models;

fn main() {
    let order = models::Order {
        customer_id: String::from("cust-1"),
        total: 100,
        status: String::from("open"),
    };
    println!("order {} total {}", order.customer_id, order.total);
}
''')
    _write(os.path.join(d, "src", "models.rs"), '''\
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Order {
    pub customer_id: String,
    pub total: i64,
    pub status: String,
}
''')
    _write(os.path.join(d, "openapi.yaml"), '''\
openapi: 3.0.0
info:
  title: Core API
  version: 1.0.0
servers:
  - url: /
paths:
  /core/orders/{id}:
    get:
      operationId: getOrder
      x-external-consumers: [partner-portal]
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      responses:
        "200":
          description: An order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customer_id, total, status]
      properties:
        customer_id:
          type: string
        total:
          type: integer
        status:
          type: string
''')
    _commit_all(d, "core: base", 0)
    return d


def _write_api(root):
    d = _init_repo(os.path.join(root, "api"))
    _write(os.path.join(d, "package.json"), '''\
{
  "name": "api",
  "version": "0.1.0",
  "private": true
}
''')
    _write(os.path.join(d, "src", "forward.ts"), '''\
// BFF: forwards GET /core/orders/{id} into GET /bff/orders/{id}.
// Incoming order: { customer_id: string; total: number }
export function forward_order(order) {
  return {
    customer_id: order.customer_id,
    total: order.total,
  };
}
''')
    _commit_all(d, "api: base", 0)
    return d


def _write_cli(root):
    d = _init_repo(os.path.join(root, "cli"))
    _write(os.path.join(d, "package.json"), '''\
{
  "name": "cli",
  "version": "0.1.0",
  "private": true
}
''')
    _write(os.path.join(d, "src", "show.ts"), '''\
// Renders an order fetched from GET /core/orders/{id} for display.
export function show_order(resp) {
  return "order for " + resp.customer_id;
}
''')
    _commit_all(d, "cli: base", 0)
    return d


def _write_billing(root):
    d = _init_repo(os.path.join(root, "billing"))
    _write(os.path.join(d, "requirements.txt"), '''\
httpx==0.27.0
''')
    _write(os.path.join(d, "src", "main.py"), '''\
"""Billing service: builds invoices from orders fetched through the BFF."""
import os
from typing import Any

import httpx

ORDERS_URL = os.environ.get("ORDERS_URL", "http://bff.invalid")


def fetch_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/bff/orders/{order_id}")
    return r.json()


def build_invoice(order_id: str) -> dict[str, Any]:
    data = fetch_order(order_id)
    return {"customer_id": data["customer_id"], "amount": data["total"]}


def get_invoice(invoice_id: str) -> dict[str, Any]:
    return build_invoice(invoice_id)
''')
    _commit_all(d, "billing: base", 0)
    return d


def _write_notify(root):
    d = _init_repo(os.path.join(root, "notify"))
    _write(os.path.join(d, "requirements.txt"), '''\
httpx==0.27.0
''')
    _write(os.path.join(d, "src", "notify.py"), '''\
"""Notification sender — notices built from billing's invoices."""
import os
from typing import Any

import httpx

INVOICES_URL = os.environ.get("INVOICES_URL", "http://billing.invalid")

# The customer field name on the wire is assembled at runtime from
# deployment config; nothing in this repository pins the suffix.
SUFFIX = "id"


def fetch_invoice(invoice_id: str) -> dict[str, Any]:
    r = httpx.get(f"{INVOICES_URL}/invoices/{invoice_id}")
    return r.json()


def send_notice(invoice_id: str) -> dict[str, str]:
    resp = fetch_invoice(invoice_id)
    name = resp.get("customer_" + SUFFIX)
    return {"to": "customer", "body": "invoice for " + str(name)}
''')
    _commit_all(d, "notify: base", 0)
    return d


def _write_reporting(root):
    d = _init_repo(os.path.join(root, "reporting"))
    _write(os.path.join(d, "src", "report_invoice.py"), '''\
"""Internal invoices for manually-adjusted orders (CSV report, no APIs)."""


def build_invoice(order_ref: str, customer_name: str, amount: int) -> dict[str, object]:
    # Built entirely from a local CSV row — this invoice never touches
    # any chain endpoint. Same name as billing's build_invoice on purpose.
    return {
        "invoice_id": f"manual-{order_ref}",
        "customer": customer_name,
        "total": amount,
        "source": "csv",
    }
''')
    _commit_all(d, "reporting: base", 0)
    return d


def _write_catalog(root):
    d = _init_repo(os.path.join(root, "catalog"))
    _write(os.path.join(d, "src", "catalog.py"), '''\
"""Catalog summaries built from core's orders — but only the total."""
import json
import os
import urllib.request
from typing import Any

CORE_URL = os.environ.get("CORE_URL", "http://core.invalid")


def fetch_order(order_id: str) -> dict[str, Any]:
    with urllib.request.urlopen(f"{CORE_URL}/core/orders/{order_id}") as resp:
        return json.load(resp)


def summarize(order_id: str) -> dict[str, Any]:
    resp = fetch_order(order_id)
    return {"total": resp["total"]}
''')
    _commit_all(d, "catalog: base", 0)
    return d


def _write_repos_yaml(root):
    # Roles copied verbatim from ground_truth.json. Built line-by-line
    # (not one f-string) because several roles contain braces.
    lines = ["data_dir: %s\nrepos:\n" % os.path.join(root, ".lain-data")]
    for rid, lang, role in (
            ("core", "rust",
             "SEED — serves GET /core/orders/{id} with response field customer_id; the change renames it to customer_ref (serde rename, same type)"),
            ("api", "typescript",
             "BFF — calls core's endpoint, forwards the field in its own GET /bff/orders/{id}"),
            ("cli", "typescript",
             "consumer — calls core's endpoint directly, reads resp.customer_id for display"),
            ("billing", "python",
             "consumer+forwarder — reads the field from api's response, re-emits it in GET /invoices/{id}"),
            ("notify", "python",
             "DEEP consumer — reads it from billing's response via dynamic key resp.get('customer_' + suffix); 3 hops from core, 2 hops from any literal mention"),
            ("reporting", "python",
             "HOMONYM TRAP — has build_invoice too, builds invoices from CSV, zero calls to any chain endpoint"),
            ("catalog", "python",
             "NEAR-MISS TRAP — calls core's endpoint but reads only `total`")):
        lines.append("  - id: %s\n" % rid)
        lines.append("    language: %s\n" % lang)
        lines.append('    role: "%s"\n' % role)
        lines.append("    source:\n")
        lines.append("      type: workspace_dir\n")
        lines.append("      path: %s\n" % os.path.join(root, rid))
    _write(os.path.join(root, "repos.yaml"), "".join(lines))


# ── build / apply-edit ──────────────────────────────────────────────────────

def build(dest):
    for fn in (_write_core, _write_api, _write_cli, _write_billing,
               _write_notify, _write_reporting, _write_catalog):
        fn(dest)
    _write_repos_yaml(dest)
    os.makedirs(os.path.join(dest, "consensus"), exist_ok=True)
    for repo, body, name, note in (
            ("core", CORE_TEST, "test_contract_artifact.py",
             "red until the rename lands"),
            ("api", API_TEST, "test_forward_contract.py",
             "red until the rename lands"),
            ("cli", CLI_TEST, "test_show_contract.py",
             "red until the rename lands"),
            ("billing", BILLING_TEST, "test_invoice_contract.py",
             "red until the rename lands"),
            ("notify", NOTIFY_TEST, "test_notice_contract.py",
             "red until the rename lands"),
            ("reporting", REPORTING_TEST, "test_report_standalone.py",
             "green throughout — homonym trap"),
            ("catalog", CATALOG_TEST, "test_catalog_total.py",
             "green throughout — near-miss trap")):
        _write(os.path.join(dest, repo, "tests", name), body)
        _commit_all(os.path.join(dest, repo),
                    f"tests: swarm-deepchain oracle ({note})", 1)
    return dest


def _replace(path, old, new, what):
    with open(path) as f:
        body = f.read()
    fixed = body.replace(old, new)
    if fixed == body:
        raise SystemExit(f"apply-edit: expected {what} in {path}")
    with open(path, "w") as f:
        f.write(fixed)


def apply_edit(dest):
    """The scripted change: provider-side rename (code + openapi), then
    the consumer fixes the oracle demands along the chain — the BFF
    forward, the CLI read, the billing re-emit, and notify's dynamic
    suffix. The traps (reporting, catalog) and the external consumer
    (partner-portal) are deliberately untouched."""
    _replace(os.path.join(dest, "core", "src", "models.rs"),
             "    pub customer_id: String,",
             '    #[serde(rename = "customer_ref")]\n    pub customer_id: String,',
             "the Order struct's customer_id field")
    _replace(os.path.join(dest, "core", "openapi.yaml"),
             "      required: [customer_id, total, status]\n"
             "      properties:\n        customer_id:\n",
             "      required: [customer_ref, total, status]\n"
             "      properties:\n        customer_ref:\n",
             "the openapi Order schema's customer_id property")
    _replace(os.path.join(dest, "api", "src", "forward.ts"),
             "customer_id", "customer_ref",
             "the customer_id read/forward in forward_order")
    _replace(os.path.join(dest, "cli", "src", "show.ts"),
             "resp.customer_id", "resp.customer_ref",
             "the resp.customer_id read in show_order")
    _replace(os.path.join(dest, "billing", "src", "main.py"),
             '{"customer_id": data["customer_id"]',
             '{"customer_ref": data["customer_ref"]',
             "the customer_id read/re-emit in build_invoice")
    _replace(os.path.join(dest, "notify", "src", "notify.py"),
             'SUFFIX = "id"', 'SUFFIX = "ref"',
             "the dynamic-key suffix in send_notice")
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
