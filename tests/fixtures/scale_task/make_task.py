#!/usr/bin/env python3
"""Build the scale/homonym fixture — the program's decisive experiment.

Six deterministic git repos around ONE contract change: orders renames
the RESPONSE field `customer_id` -> `customer_ref` (same type). The
oracle (ground_truth.json, declared BEFORE any query) is about harm:
- gateway (TS BFF) reads the field directly        -> must follow
- billing (Python) reads it through gateway's API  -> must follow
- analytics (Python) reads it dynamically          -> must be DECLARED
  unknown (missing/needs-investigation), never silently dropped
- reporting (Python) has its own `build_invoice`   -> HOMONYM TRAP
- catalog (Python) reads `total` from the same     -> NEAR-MISS TRAP
  orders endpoint, not the renamed field

Oracle tests (stdlib-only unittest; frameworks stubbed, contract
artifacts read as text where the language has no runtime here) live in
orders, gateway and billing — red before `apply-edit`, green after.

Usage:
    make_task.py build <dir>        # 6 repos + oracle tests + commits
    make_task.py apply-edit <dir>   # the scripted change (graph mode)
"""
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

# Deterministic commit identity/dates (same base epoch as
# scripts/contracts-fixture.sh): refs and tree bytes are stable.
BASE_EPOCH = 1736140800  # 2025-01-06T09:00:00Z

ORDERS_TEST = '''\
"""Contract-artifact oracle for the change: orders' RESPONSE schema must
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
        text = _read("src/orders/models.rs")
        order = _block(text, "pub struct Order", "\\n}")
        self.assertIn('rename = "customer_ref"', order)

    def test_openapi_order_schema_uses_new_name(self):
        text = _read("openapi.yaml")
        order = _block(text, "    Order:")
        self.assertIn("customer_ref", order)
        self.assertNotIn("customer_id", order)


if __name__ == "__main__":
    unittest.main()
'''

GATEWAY_TEST = '''\
"""Contract-artifact oracle for the change: the BFF must read orders'
NEW response field name and forward it under that same wire name.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class ForwardOrderFollowsTheRenamedField(unittest.TestCase):
    def test_reads_and_forwards_the_new_wire_name(self):
        with open(os.path.join(ROOT, "src", "forward.ts")) as f:
            text = f.read()
        self.assertIn("customer_ref", text)
        self.assertNotIn("customer_id", text)


if __name__ == "__main__":
    unittest.main()
'''

BILLING_TEST = '''\
"""Behavioral oracle for the change: with gateway's NEW forwarded field
name (orders renamed it), build_invoice must still produce a complete
invoice. This is the INDIRECT consumer — its evidence chain runs
gateway -> orders.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

os.environ.setdefault("GATEWAY_URL", "http://gateway.invalid")


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


class BuildInvoiceReadsTheForwardedField(unittest.TestCase):
    def test_reads_the_new_field_name(self):
        # gateway's response after the rename — the field orders renamed
        # is forwarded under its new wire name; billing must follow it.
        httpx_stub.get = lambda url: _Resp({"customer_ref": "cust-1", "total": 5})
        main.httpx = httpx_stub
        invoice = main.build_invoice("ord-1")
        self.assertEqual(invoice.customer_id, "cust-1")
        self.assertEqual(invoice.total, 5)


if __name__ == "__main__":
    unittest.main()
'''


def _git(dir_, *args, offset_days=0):
    env = dict(os.environ)
    stamp = f"{BASE_EPOCH + offset_days * 86400} +0000"
    env.update(
        GIT_AUTHOR_NAME="scale-task-fixture", GIT_AUTHOR_EMAIL="fixture@lain.local",
        GIT_COMMITTER_NAME="scale-task-fixture", GIT_COMMITTER_EMAIL="fixture@lain.local",
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

def _write_orders(root):
    d = _init_repo(os.path.join(root, "orders"))
    _write(os.path.join(d, "Cargo.toml"), '''\
[package]
name = "orders"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = "0.7"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
''')
    _write(os.path.join(d, "src", "main.rs"), '''\
use axum::{routing::get, Router};

mod orders;

use orders::list::get_order;

#[tokio::main]
async fn main() {
    let app = Router::new().route("/api/orders/:id", get(get_order));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
''')
    _write(os.path.join(d, "src", "orders", "mod.rs"), '''\
pub mod list;
pub mod models;
''')
    _write(os.path.join(d, "src", "orders", "models.rs"), '''\
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Order {
    pub customer_id: String,
    pub total: i64,
    pub status: OrderStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Open,
    Paid,
}
''')
    _write(os.path.join(d, "src", "orders", "list.rs"), '''\
use axum::{extract::Path, Json};

use super::models::{Order, OrderStatus};

pub async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
    })
}
''')
    _write(os.path.join(d, "openapi.yaml"), '''\
openapi: 3.0.0
info:
  title: Orders API
  version: 1.0.0
servers:
  - url: /
paths:
  /api/orders/{id}:
    get:
      operationId: getOrder
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
          enum: [open, paid]
''')
    _commit_all(d, "orders: base", 0)
    return d


def _write_gateway(root):
    d = _init_repo(os.path.join(root, "gateway"))
    _write(os.path.join(d, "package.json"), '''\
{
  "name": "gateway",
  "version": "0.1.0",
  "private": true,
  "dependencies": {
    "express": "^4.18.0"
  }
}
''')
    _write(os.path.join(d, "src", "forward.ts"), '''\
import express from "express";

const ORDERS_URL = process.env.ORDERS_URL;
const PORT = 3002;

// BFF: fetches orders' GET /api/orders/{id} and forwards the customer
// field (and total) in the gateway's own response.
export async function forward_order(id: string): Promise<Record<string, unknown>> {
  const res = await fetch(`${ORDERS_URL}/api/orders/${id}`);
  const order = (await res.json()) as { customer_id: string; total: number };
  return {
    customer_id: order.customer_id,
    total: order.total,
  };
}

const app = express();
app.get("/bff/orders/:id", async (req, res) => {
  res.json(await forward_order(req.params.id));
});
app.listen(PORT);
''')
    _commit_all(d, "gateway: base", 0)
    return d


def _write_billing(root):
    d = _init_repo(os.path.join(root, "billing"))
    _write(os.path.join(d, "requirements.txt"), '''\
fastapi==0.110.0
httpx==0.27.0
pydantic==2.6.0
uvicorn==0.29.0
''')
    _write(os.path.join(d, "src", "main.py"), '''\
"""Billing service: builds invoices from orders through the gateway BFF."""
import os
from typing import Any

import httpx
from fastapi import FastAPI
from pydantic import BaseModel

GATEWAY_URL = os.environ["GATEWAY_URL"]

app = FastAPI()


class Invoice(BaseModel):
    id: str
    customer_id: str
    total: int


def fetch_forwarded_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(f"{GATEWAY_URL}/bff/orders/{order_id}")
    return r.json()


def build_invoice(order_id: str) -> Invoice:
    row = fetch_forwarded_order(order_id)
    return Invoice(
        id=order_id,
        customer_id=row["customer_id"],
        total=row["total"],
    )


@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: str) -> Invoice:
    return build_invoice(order_id=invoice_id)
''')
    _commit_all(d, "billing: base", 0)
    return d


def _write_reporting(root):
    d = _init_repo(os.path.join(root, "reporting"))
    _write(os.path.join(d, "src", "report_invoice.py"), '''\
"""Internal invoices for manually-adjusted orders (no orders API)."""


def build_invoice(order_ref: str, customer_name: str, amount: int) -> dict[str, object]:
    # Built entirely from local data — this invoice never touches the
    # orders service. Same name as billing's build_invoice on purpose.
    return {
        "invoice_id": f"manual-{order_ref}",
        "customer": customer_name,
        "total": amount,
        "source": "manual",
    }
''')
    _commit_all(d, "reporting: base", 0)
    return d


def _write_catalog(root):
    d = _init_repo(os.path.join(root, "catalog"))
    _write(os.path.join(d, "requirements.txt"), '''\
httpx==0.27.0
''')
    _write(os.path.join(d, "src", "catalog.py"), '''\
"""Catalog summaries built from orders — but only the total."""
import os

import httpx

ORDERS_URL = os.environ["ORDERS_URL"]


def summarize(order_id: str) -> dict[str, object]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}")
    order = r.json()
    return {"order_id": order_id, "total": order["total"]}
''')
    _commit_all(d, "catalog: base", 0)
    return d


def _write_analytics(root):
    d = _init_repo(os.path.join(root, "analytics"))
    _write(os.path.join(d, "requirements.txt"), '''\
httpx==0.27.0
''')
    _write(os.path.join(d, "src", "analytics.py"), '''\
"""Customer analytics over orders — URL and field are both dynamic."""
from typing import Any

import httpx


# The base is assembled at runtime from deployment config; nothing in
# this repository pins it to the orders service.
def _orders_base() -> str:
    return _DEPLOYED_ORDERS_BASE


def report(order_id: str, key: str) -> dict[str, Any]:
    base = _orders_base()
    r = httpx.get(f"{base}/api/orders/{order_id}")
    resp = r.json()
    return {"customer": resp["customer_" + key], "total": resp["total"]}
''')
    _commit_all(d, "analytics: base", 0)
    return d


def _write_repos_yaml(root):
    _write(os.path.join(root, "repos.yaml"), f'''\
data_dir: {os.path.join(root, ".lain-data")}
repos:
  - id: orders
    source:
      type: workspace_dir
      path: {os.path.join(root, "orders")}
  - id: gateway
    source:
      type: workspace_dir
      path: {os.path.join(root, "gateway")}
  - id: billing
    source:
      type: workspace_dir
      path: {os.path.join(root, "billing")}
  - id: reporting
    source:
      type: workspace_dir
      path: {os.path.join(root, "reporting")}
  - id: catalog
    source:
      type: workspace_dir
      path: {os.path.join(root, "catalog")}
  - id: analytics
    source:
      type: workspace_dir
      path: {os.path.join(root, "analytics")}
services:
  - name: orders
    repo: orders
    paths: []
    env: [ORDERS_URL]
  - name: gateway
    repo: gateway
    paths: []
    env: [GATEWAY_URL]
  - name: billing
    repo: billing
    paths: []
    env: [BILLING_URL]
  - name: reporting
    repo: reporting
    paths: []
  - name: catalog
    repo: catalog
    paths: []
  - name: analytics
    repo: analytics
    paths: []
http_clients: []
generic_keys:
  - GET /health
  - GET /healthz
  - GET /ready
  - GET /readyz
  - GET /live
  - GET /livez
  - GET /ping
  - GET /status
  - GET /version
  - GET /metrics
bindings: []
''')


# ── build / apply-edit ──────────────────────────────────────────────────────

def build(dest):
    for fn in (_write_orders, _write_gateway, _write_billing,
               _write_reporting, _write_catalog, _write_analytics):
        fn(dest)
    _write_repos_yaml(dest)
    for repo, body, name in (
            ("orders", ORDERS_TEST, "test_contract_artifact.py"),
            ("gateway", GATEWAY_TEST, "test_forward_contract.py"),
            ("billing", BILLING_TEST, "test_invoice_contract.py")):
        _write(os.path.join(dest, repo, "tests", name), body)
        _commit_all(os.path.join(dest, repo),
                    "tests: scale-task oracle (red until the rename lands)", 1)
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
    the two consumer fixes the oracle demands — the direct BFF read and
    the indirect billing read. The traps (reporting, catalog) and the
    unresolvable consumer (analytics) are deliberately untouched."""
    _replace(os.path.join(dest, "orders", "src", "orders", "models.rs"),
             'pub struct Order {\n    pub customer_id: String,',
             'pub struct Order {\n    #[serde(rename = "customer_ref")]\n'
             '    pub customer_id: String,',
             "the Order struct's customer_id field")
    _replace(os.path.join(dest, "orders", "openapi.yaml"),
             "      required: [customer_id, total, status]\n"
             "      properties:\n        customer_id:\n",
             "      required: [customer_ref, total, status]\n"
             "      properties:\n        customer_ref:\n",
             "the openapi Order schema's customer_id property")
    _replace(os.path.join(dest, "gateway", "src", "forward.ts"),
             "customer_id", "customer_ref",
             "the customer_id read/forward in forward_order")
    _replace(os.path.join(dest, "billing", "src", "main.py"),
             'customer_id=row["customer_id"]', 'customer_id=row["customer_ref"]',
             "the customer_id read in build_invoice")
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
