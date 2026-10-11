#!/usr/bin/env python3
"""Build the enterprise-SCALE fixture — the mesh plus ~70 noise repos.

This is the SCALE variant of tests/fixtures/enterprise_mesh: the SAME
ten-repo mesh (built by importing and calling the mesh builder's
build()/apply_edit()) plus SEVENTY generated noise repos. The noise
floods grep with same-name decoys — literal `customer_id` fields,
protos, events and routes — while carrying ZERO coupling to
orders-svc. The stress under test: an agent's ability to find the real
blast radius in a workspace where exhaustive reading is impractical.
The oracle (ground_truth.json, same must_find/must_not_claim as the
mesh plus affected.noise_policy / harm_metrics.noise_claims) is
declared BEFORE any run.

The 70 noise repos are generated from a fixed template table
(NOISE below): realistic service shapes across python / typescript /
go / rust / java (spread like the mesh), each with 1-3 source files,
a trivial ALWAYS-GREEN unittest oracle asserting its own behavior,
and (for some) its own protos/*.proto, openapi.yaml, or event/topic
code. At least ~40 noise repos carry the literal `customer_id`; five
define a build_invoice/sync_customer/summarize-style homonym. Every
noise repo gets its own surfaces (/v1/customers, topic <name>.events,
queue <name>, proto <slug>.proto) — never orders.proto,
order.events, order-events, or /v1/orders (build self-audits this).

repos.yaml is written by the mesh builder first (10 repos + the
orders services: block); this builder then extends `repos:` to all 80
entries (mesh first, then the noise) and leaves `services:`,
`data_dir:` and consensus/ exactly as the mesh wrote them.

apply_edit is a pure delegation to the mesh's apply_edit — the
scripted rename touches the mesh repos only; the noise is untouched.

Usage:
    make_task.py build <dir>        # 80 repos + oracle tests + commits
    make_task.py apply-edit <dir>   # the scripted change (mesh only)
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
MESH_PATH = os.path.normpath(os.path.join(HERE, os.pardir,
                                          "enterprise_mesh", "make_task.py"))


def _load_mesh(path):
    """Import the enterprise_mesh builder standalone (sibling fixture)."""
    spec = importlib.util.spec_from_file_location("enterprise_mesh_make_task",
                                                  path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


MESH = _load_mesh(MESH_PATH)
build_mesh = MESH.build
apply_edit_mesh = MESH.apply_edit
_init_repo = MESH._init_repo
_commit_all = MESH._commit_all
_write = MESH._write

# ── noise template table ────────────────────────────────────────────────────
# (name, shape). Language is derived from the shape (same spread as the
# mesh: python majority, then typescript, go, rust, java). Names are
# deterministic and never collide with the ten mesh repos.

NOISE = [
    # ── crm / customer-data family ──
    ("crm-sync", "db"),
    ("profile-api", "http"),
    ("profile-merge", "hom_sync"),
    ("customer-import", "dto"),
    ("directory-svc", "ts_proto"),
    ("lead-router", "event"),
    ("contact-store", "db"),
    ("segment-builder", "dto"),
    ("audience-merge", "ts_http"),
    ("crm-mirror", "hom_sync"),
    # ── audit / ops ──
    ("audit-log-writer", "queue"),
    ("audit-shipper", "go_http"),
    ("ops-metrics", "metrics"),
    ("change-journal", "db"),
    ("event-tap", "ts_event"),
    ("health-probe", "health"),
    ("log-rotator", "rs_serde"),
    ("config-server", "ts_dto"),
    ("secret-broker", "go_json"),
    ("feature-flags", "ts_dto"),
    # ── commerce-adjacent (never orders) ──
    ("pricing-cache", "http"),
    ("catalog-read", "db"),
    ("inventory-read", "go_http"),
    ("shipping-quotes", "event"),
    ("tax-rates", "rs_serde"),
    ("coupon-engine", "rs_http"),
    ("payment-intake", "queue"),
    ("refund-worker", "event"),
    ("invoice-lite", "hom_invoice"),
    ("quickbooks-export", "hom_invoice"),
    # ── comms ──
    ("email-gateway", "event"),
    ("sms-relay", "ts_http"),
    ("push-dispatch", "queue"),
    ("webhook-distributor", "go_json"),
    ("digest-maker", "java_dto"),
    ("notification-store", "db"),
    ("template-render", "ts_dto"),
    ("bounce-tracker", "java_dto"),
    # ── content / media ──
    ("search-indexer", "event"),
    ("document-store", "db"),
    ("media-transcoder", "rs_serde"),
    ("image-resize", "ts_clean"),
    ("thumbnailer", "health"),
    ("crawl-scheduler", "go_proto"),
    ("feed-aggregator", "ts_http"),
    ("archive-cold", "java_http"),
    # ── platform ──
    ("session-store", "db"),
    ("auth-gateway", "ts_http"),
    ("rate-limiter", "go_json"),
    ("cache-warmer", "metrics"),
    ("job-scheduler", "queue"),
    ("report-gen", "hom_summary"),
    ("export-csv", "dto"),
    ("etl-runner", "go_http"),
    ("warehouse-load", "rs_event"),
    ("metric-rollup", "rs_event"),
    # ── infra / misc ──
    ("batch-mailer", "event"),
    ("schema-registry", "ts_proto"),
    ("edge-router", "go_http"),
    ("queue-monitor", "queue"),
    ("dead-letter", "queue"),
    ("id-mint", "rs_serde"),
    ("time-sync", "java_http"),
    ("geo-lookup", "http"),
    ("currency-convert", "ts_dto"),
    ("tenant-provision", "go_json"),
    ("backup-agent", "health"),
    ("index-optimizer", "rs_http"),
    ("device-registry", "db"),
    ("consent-store", "dto"),
]

_SHAPE_LANG = {
    "db": "python", "http": "python", "event": "python", "queue": "python",
    "dto": "python", "health": "python", "metrics": "python",
    "hom_invoice": "python", "hom_sync": "python", "hom_summary": "python",
    "ts_dto": "typescript", "ts_http": "typescript", "ts_event": "typescript",
    "ts_proto": "typescript", "ts_clean": "typescript",
    "go_json": "go", "go_http": "go", "go_proto": "go",
    "rs_serde": "rust", "rs_event": "rust", "rs_http": "rust",
    "java_dto": "java", "java_http": "java",
}

# Shapes whose sources never mention customer_id (the rest all do).
_NO_CID_SHAPES = {"health", "metrics", "ts_clean"}

assert len(NOISE) == 70, "noise table must have exactly 70 entries"
assert len({name for name, _ in NOISE}) == 70, "noise names must be unique"
assert all(shape in _SHAPE_LANG for _, shape in NOISE), "unknown shape"
assert sum(1 for _, s in NOISE if s.startswith("hom_")) == 5, \
    "exactly five same-name homonyms"
assert sum(1 for _, s in NOISE if s not in _NO_CID_SHAPES) >= 40, \
    "at least 40 noise repos carry customer_id"

# What the builder self-audits after generation: no noise file may
# reference any orders-svc contract surface.
FORBIDDEN_IN_NOISE = ("orders.proto", "order.events", "order-events",
                      "/v1/orders")


def _slug(name):
    return name.replace("-", "_")


def _camel(name):
    return "".join(part.capitalize() for part in name.split("-"))


def _proto_body(slug):
    # Built with % formatting — the body contains braces.
    return '''\
syntax = "proto3";

package %s;

message Customer {
  string customer_id = 1;
  string email = 2;
}

service %s {
  rpc GetCustomer(GetCustomerRequest) returns (Customer);
}

message GetCustomerRequest {
  string id = 1;
}
''' % (slug, _camel(slug))


def _openapi_body(name):
    return '''\
openapi: 3.0.0
info:
  title: %s API
  version: 1.0.0
servers:
  - url: /
paths:
  /v1/customers/{id}:
    get:
      operationId: getCustomer
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      responses:
        "200":
          description: A customer
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Customer"
components:
  schemas:
    Customer:
      type: object
      required: [customer_id, email]
      properties:
        customer_id:
          type: string
        email:
          type: string
''' % name


def _artifact_test(name, checks, doc):
    """A green unittest asserting this repo's own artifacts (text)."""
    lines = [
        '"""%s — %s' % (name, doc),
        "",
        "Standalone green oracle: asserts this repo's own artifacts.",
        "ALWAYS GREEN — the scripted change never touches noise repos.",
        '"""',
        "import os",
        "import unittest",
        "",
        "ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))",
        "",
        "",
        "class OwnArtifacts(unittest.TestCase):",
    ]
    for i, (rel, needle) in enumerate(checks, 1):
        lines += [
            "    def test_%d(self):" % i,
            '        with open(os.path.join(ROOT, "%s")) as f:' % rel,
            "            text = f.read()",
            "        self.assertIn(%r, text)" % needle,
            "",
        ]
    lines += ["", 'if __name__ == "__main__":', "    unittest.main()", ""]
    return "\n".join(lines)


# ── python shape sources + behavioral tests ─────────────────────────────────

def _sh_db(d, name):
    _write(os.path.join(d, "src", "store.py"), '''\
"""Customer store — projects rows of its own table (column customer_id)."""
from typing import Any


def project(row: dict[str, Any]) -> dict[str, Any]:
    return {"crm_id": row["customer_id"], "name": row.get("name", "")}


def key_of(row: dict[str, Any]) -> str:
    return row["customer_id"]
''')
    return "test_store_behavior.py", '''\
"""Green oracle: projects its own customer_id column. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import store  # noqa: E402


class ProjectOwnsItsColumn(unittest.TestCase):
    def test_projects_its_own_column(self):
        out = store.project({"customer_id": "C-7", "name": "Ada"})
        self.assertEqual(out["crm_id"], "C-7")
        self.assertEqual(store.key_of({"customer_id": "C-7"}), "C-7")


if __name__ == "__main__":
    unittest.main()
'''


def _sh_http(d, name):
    _write(os.path.join(d, "requirements.txt"), "httpx==0.27.0\n")
    _write(os.path.join(d, "src", "client.py"), '''\
"""Customer profile client — GET /v1/customers/{id} on its own host.

The host comes from the service's OWN env var CUSTOMERS_BASE; nothing
here binds to any other service.
"""
import os
from typing import Any

import httpx

BASE = os.environ.get("CUSTOMERS_BASE", "http://customers.internal")


def fetch_customer(customer_id: str) -> dict[str, Any]:
    r = httpx.get(f"{BASE}/v1/customers/{customer_id}")
    return r.json()


def profile(customer_id: str) -> dict[str, Any]:
    data = fetch_customer(customer_id)
    return {"customer_id": data.get("customer_id", customer_id),
            "name": data.get("name", "")}
''')
    _write(os.path.join(d, "openapi.yaml"), _openapi_body(name))
    return "test_profile_behavior.py", '''\
"""Green oracle: profiles its own /v1/customers payload. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("httpx", types.ModuleType("httpx"))

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import client  # noqa: E402


class ProfileReadsItsOwnField(unittest.TestCase):
    def test_profile_returns_the_customer(self):
        client.fetch_customer = \\
            lambda cid: {"customer_id": cid, "name": "Ada"}
        out = client.profile("C-1")
        self.assertEqual(out["customer_id"], "C-1")
        self.assertEqual(out["name"], "Ada")

    def test_documents_its_own_route(self):
        with open(os.path.join(ROOT, "openapi.yaml")) as f:
            text = f.read()
        self.assertIn("/v1/customers/{id}", text)


if __name__ == "__main__":
    unittest.main()
'''


def _sh_event(d, name):
    _write(os.path.join(d, "requirements.txt"), "confluent-kafka==2.3.0\n")
    _write(os.path.join(d, "src", "publisher.py"), '''\
"""Publishes customer events on its OWN Kafka topic <name>.events."""
import json
from typing import Any

from confluent_kafka import Producer

TOPIC = "%s.events"


def build_payload(customer_id: str, amount: int) -> dict[str, Any]:
    return {"customer_id": customer_id, "amount": amount}


def publish(producer: Producer, customer_id: str, amount: int) -> None:
    producer.produce(TOPIC, json.dumps(build_payload(customer_id, amount)))
''' % name)
    return "test_publish_behavior.py", '''\
"""Green oracle: publishes its own topic payload. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import types
import unittest

_stub = types.ModuleType("confluent_kafka")


class _Producer:
    def __init__(self, *args, **kwargs):
        self.sent = []

    def produce(self, topic, value):
        self.sent.append((topic, value))


_stub.Producer = _Producer
sys.modules.setdefault("confluent_kafka", _stub)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import publisher  # noqa: E402


class PublishesItsOwnEvent(unittest.TestCase):
    def test_payload_carries_its_own_field(self):
        p = publisher.build_payload("C-1", 5)
        self.assertEqual(p["customer_id"], "C-1")
        self.assertEqual(p["amount"], 5)

    def test_publishes_to_its_own_topic(self):
        prod = _Producer()
        publisher.publish(prod, "C-1", 5)
        self.assertEqual(prod.sent[0][0], publisher.TOPIC)
        self.assertTrue(publisher.TOPIC.endswith(".events"))


if __name__ == "__main__":
    unittest.main()
'''


def _sh_queue(d, name):
    _write(os.path.join(d, "requirements.txt"), "pika==1.3.2\n")
    _write(os.path.join(d, "src", "consumer.py"), '''\
"""Consumes its OWN AMQP queue %s (pika) — its own bus,
its own payload key. No Kafka, no HTTP.
"""
import pika
from typing import Any

QUEUE = "%s"


def connect(url: str):
    return pika.BlockingConnection(url)


def bind_queue(channel) -> None:
    channel.queue_bind(queue=QUEUE, exchange="platform", routing_key=QUEUE)


def handle_delivery(payload: dict[str, Any]) -> dict[str, Any]:
    return {"customer_id": payload["customer_id"], "recorded": True}
''' % (name, name))
    return "test_delivery_behavior.py", '''\
"""Green oracle: records its own queue payload. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("pika", types.ModuleType("pika"))

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import consumer  # noqa: E402


class HandleDeliveryRecordsItsOwnField(unittest.TestCase):
    def test_records_the_payload_field(self):
        out = consumer.handle_delivery({"customer_id": "C-3", "n": 1})
        self.assertEqual(out["customer_id"], "C-3")
        self.assertTrue(out["recorded"])

    def test_binds_its_own_queue(self):
        self.assertTrue(consumer.QUEUE)


if __name__ == "__main__":
    unittest.main()
'''


def _sh_dto(d, name):
    _write(os.path.join(d, "src", "schemas.py"), '''\
"""Wire DTO normalization for this service's own customer form."""
from typing import Any


def normalize(form: dict[str, Any]) -> dict[str, Any]:
    return {"customer_id": str(form.get("customer_id", "")).strip(),
            "email": form.get("email", "")}
''')
    return "test_normalize_behavior.py", '''\
"""Green oracle: normalizes its own form field. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import schemas  # noqa: E402


class NormalizeOwnsItsField(unittest.TestCase):
    def test_normalizes_its_own_field(self):
        out = schemas.normalize({"customer_id": " C-4 ", "email": "a@b"})
        self.assertEqual(out["customer_id"], "C-4")
        self.assertEqual(out["email"], "a@b")


if __name__ == "__main__":
    unittest.main()
'''


def _sh_health(d, name):
    _write(os.path.join(d, "src", "health.py"), '''\
"""Liveness probe handler for this service."""


def status() -> dict:
    return {"ok": True, "service": "%s"}
''' % name)
    return "test_health_behavior.py", '''\
"""Green oracle: its own probe answers ok. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import health  # noqa: E402


class ProbeAnswersOk(unittest.TestCase):
    def test_status_ok(self):
        self.assertTrue(health.status()["ok"])


if __name__ == "__main__":
    unittest.main()
'''


def _sh_metrics(d, name):
    _write(os.path.join(d, "src", "metrics.py"), '''\
"""Counter registry for this service's own exporter."""
from typing import Any

COUNTS: dict[str, Any] = {}


def bump(name: str, n: int = 1) -> int:
    COUNTS[name] = COUNTS.get(name, 0) + n
    return COUNTS[name]
''')
    return "test_metrics_behavior.py", '''\
"""Green oracle: its own counters bump. ALWAYS GREEN.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import metrics  # noqa: E402


class CountersBump(unittest.TestCase):
    def test_bump_accumulates(self):
        metrics.COUNTS.clear()
        metrics.bump("starts")
        self.assertEqual(metrics.bump("starts"), 2)


if __name__ == "__main__":
    unittest.main()
'''


def _sh_hom_invoice(d, name):
    _write(os.path.join(d, "src", "billing.py"), '''\
"""Invoice builder from this service's OWN charge lines.

The build_invoice name is a pure coincidence with any other billing
code — there is no shared caller, queue, route or proto.
"""
from typing import Any


def build_invoice(customer_id: str, lines: list[int],
                  tax_rate: float = 0.0) -> dict[str, Any]:
    subtotal = sum(lines)
    return {"customer_id": customer_id, "subtotal": subtotal,
            "total": subtotal + int(subtotal * tax_rate)}
''')
    return "test_invoice_behavior.py", '''\
"""Green oracle: its own build_invoice. ALWAYS GREEN — the name is a
homonym trap for grep, not a contract edge.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import billing  # noqa: E402


class BuildInvoiceOwnsItsInputs(unittest.TestCase):
    def test_invoices_from_its_own_lines(self):
        inv = billing.build_invoice("C-1", [10, 20], 0.1)
        self.assertEqual(inv["customer_id"], "C-1")
        self.assertEqual(inv["total"], 33)


if __name__ == "__main__":
    unittest.main()
'''


def _sh_hom_sync(d, name):
    _write(os.path.join(d, "src", "mirror.py"), '''\
"""Mirror sync from this service's OWN staging table.

The sync_customer name is a pure coincidence with any other CRM code
— it reads its own rows and talks to nobody.
"""
from typing import Any


def sync_customer(row: dict[str, Any]) -> dict[str, Any]:
    return {"mirror_id": row["customer_id"], "synced": True}
''')
    return "test_sync_behavior.py", '''\
"""Green oracle: its own sync_customer. ALWAYS GREEN — the name is a
homonym trap for grep, not a contract edge.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import mirror  # noqa: E402


class SyncOwnsItsRows(unittest.TestCase):
    def test_syncs_its_own_row(self):
        out = mirror.sync_customer({"customer_id": "C-9", "name": "Ada"})
        self.assertEqual(out["mirror_id"], "C-9")
        self.assertTrue(out["synced"])


if __name__ == "__main__":
    unittest.main()
'''


def _sh_hom_summary(d, name):
    _write(os.path.join(d, "src", "report.py"), '''\
"""Daily summary over this service's OWN rows.

The summarize name is a pure coincidence with any other summary code
— the input is local, the output is a dict.
"""
from typing import Any


def summarize(records: list[dict[str, Any]]) -> dict[str, Any]:
    return {"rows": len(records),
            "customer_ids": sorted(r["customer_id"] for r in records)}
''')
    return "test_summary_behavior.py", '''\
"""Green oracle: its own summarize. ALWAYS GREEN — the name is a
homonym trap for grep, not a contract edge.

The scripted change never touches noise repos.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import report  # noqa: E402


class SummarizeOwnsItsRows(unittest.TestCase):
    def test_sums_its_own_rows(self):
        out = report.summarize([{"customer_id": "C-2"},
                                {"customer_id": "C-1"}])
        self.assertEqual(out["rows"], 2)
        self.assertEqual(out["customer_ids"], ["C-1", "C-2"])


if __name__ == "__main__":
    unittest.main()
'''


# ── typescript shape sources (artifact tests) ───────────────────────────────

def _ts_package(d, name):
    _write(os.path.join(d, "package.json"), '''\
{
  "name": "%s",
  "version": "0.1.0",
  "private": true
}
''' % name)


def _sh_ts_dto(d, name):
    _ts_package(d, name)
    _write(os.path.join(d, "src", "types.ts"), '''\
// %s — customer DTO serialization for its own /v1/customers payload.
export interface Customer {
  customer_id: string;
  email: string;
}

export function toPayload(c: Customer): string {
  return JSON.stringify({ customer_id: c.customer_id, email: c.email });
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/types.ts", "customer_id"),
        ("src/types.ts", "toPayload"),
    ], "own DTO carries its own field")


def _sh_ts_http(d, name):
    _ts_package(d, name)
    _write(os.path.join(d, "src", "api.ts"), '''\
// %s — fetches a customer from its OWN gateway: GET /v1/customers/{id}.
const BASE = process.env.CUSTOMERS_BASE ?? "http://customers.internal";

export async function fetchCustomer(
  id: string,
): Promise<Record<string, unknown>> {
  const r = await fetch(`${BASE}/v1/customers/${id}`);
  return (await r.json()) as Record<string, unknown>;
}

export function pick(payload: Record<string, unknown>) {
  return { customer_id: payload["customer_id"] };
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/api.ts", "/v1/customers/"),
        ("src/api.ts", 'payload["customer_id"]'),
    ], "own HTTP client reads its own field")


def _sh_ts_event(d, name):
    _ts_package(d, name)
    _write(os.path.join(d, "src", "stream.ts"), '''\
// %s — consumes its OWN Kafka topic %s.events (kafkajs).
import { Kafka } from "kafkajs";

const TOPIC = "%s.events";
const kafka = new Kafka({ clientId: "%s", brokers: ["kafka:9092"] });

export async function start(): Promise<void> {
  const consumer = kafka.consumer({ groupId: "%s" });
  await consumer.connect();
  await consumer.subscribe({ topics: [TOPIC] });
  await consumer.run({
    eachMessage: async ({ message }) => {
      handle(JSON.parse(message.value?.toString() ?? "{}"));
    },
  });
}

export function handle(payload: Record<string, unknown>) {
  return { customer: payload["customer_id"] };
}
''' % (name, name, name, name, name))
    return "test_artifacts.py", _artifact_test(name, [
        ("src/stream.ts", "%s.events" % name),
        ("src/stream.ts", 'payload["customer_id"]'),
    ], "own stream consumer reads its own field")


def _sh_ts_proto(d, name):
    _ts_package(d, name)
    slug = _slug(name)
    _write(os.path.join(d, "protos", "%s.proto" % slug), _proto_body(slug))
    _write(os.path.join(d, "src", "show.ts"), '''\
// %s — renders a Customer message from its OWN %s.proto.
export function show(c: { customerId: string }) {
  return "customer " + c.customerId;
}
''' % (name, slug))
    return "test_artifacts.py", _artifact_test(name, [
        ("protos/%s.proto" % slug, "string customer_id = 1;"),
        ("protos/%s.proto" % slug, "service %s" % _camel(slug)),
        ("src/show.ts", "export function show"),
    ], "own proto carries its own field")


def _sh_ts_clean(d, name):
    _ts_package(d, name)
    _write(os.path.join(d, "src", "util.ts"), '''\
// %s — small retry helpers for this service.
export function backoffMs(attempt: number): number {
  return Math.min(1000 * 2 ** attempt, 30000);
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/util.ts", "backoffMs"),
    ], "own helper exists")


# ── go shape sources (artifact tests) ───────────────────────────────────────

def _go_mod(d, name):
    _write(os.path.join(d, "go.mod"), "module %s\n\ngo 1.22\n" % name)


def _sh_go_json(d, name):
    _go_mod(d, name)
    _write(os.path.join(d, "src", "handler.go"), '''\
// Package handler serves %s's own customer directory over HTTP.
package handler

type Customer struct {
	CustomerID string `json:"customer_id" db:"customer_id"`
	Email      string `json:"email"`
}

func Profile(c Customer) string {
	return c.CustomerID + " <" + c.Email + ">"
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/handler.go", 'json:"customer_id"'),
        ("src/handler.go", "func Profile"),
    ], "own DTO carries its own field")


def _sh_go_http(d, name):
    _go_mod(d, name)
    _write(os.path.join(d, "src", "client.go"), '''\
// Package client calls %s's OWN customer gateway:
// GET /v1/customers/{id}. The host comes from CUSTOMERS_BASE.
package client

import (
	"net/http"
	"os"
)

type Customer struct {
	CustomerID string `json:"customer_id"`
}

func Fetch(id string) (*http.Response, error) {
	base := os.Getenv("CUSTOMERS_BASE")
	return http.Get(base + "/v1/customers/" + id)
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/client.go", "/v1/customers/"),
        ("src/client.go", 'json:"customer_id"'),
    ], "own HTTP client reads its own field")


def _sh_go_proto(d, name):
    _go_mod(d, name)
    slug = _slug(name)
    _write(os.path.join(d, "protos", "%s.proto" % slug), _proto_body(slug))
    _write(os.path.join(d, "src", "client.go"), '''\
// Package client dials %s's OWN %s service (generated stubs
// checked in elsewhere). It reads Customer.customer_id from its own
// message — no external proto is involved.
package client

import (
	"context"
	"os"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

type Customer struct {
	CustomerId string
}

func GetCustomer(ctx context.Context) (string, error) {
	target := os.Getenv("CUSTOMERS_GRPC_ADDR")
	conn, err := grpc.NewClient(target, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return "", err
	}
	_ = conn
	return "", nil
}
''' % (name, slug))
    return "test_artifacts.py", _artifact_test(name, [
        ("protos/%s.proto" % slug, "string customer_id = 1;"),
        ("src/client.go", "Customer.customer_id"),
        ("src/client.go", "func GetCustomer"),
    ], "own proto carries its own field")


# ── rust shape sources (artifact tests) ─────────────────────────────────────

def _rust_cargo(d, name):
    _write(os.path.join(d, "Cargo.toml"), '''\
[package]
name = "%s"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
''' % name)


def _sh_rs_serde(d, name):
    _rust_cargo(d, name)
    _write(os.path.join(d, "src", "model.rs"), '''\
//! %s's own customer model (serde).
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Customer {
    pub customer_id: String,
    pub email: String,
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/model.rs", "pub customer_id: String"),
    ], "own model carries its own field")


def _sh_rs_event(d, name):
    _rust_cargo(d, name)
    _write(os.path.join(d, "src", "events.rs"), '''\
//! %s's own event payload (serde_json) — topic %s.events.
use serde_json::json;

pub const TOPIC: &str = "%s.events";

pub fn payload(customer_id: &str, amount: i64) -> String {
    json!({"customer_id": customer_id, "amount": amount}).to_string()
}
''' % (name, name, name))
    return "test_artifacts.py", _artifact_test(name, [
        ("src/events.rs", "%s.events" % name),
        ("src/events.rs", 'json!({"customer_id":'),
    ], "own event payload carries its own field")


def _sh_rs_http(d, name):
    _rust_cargo(d, name)
    _write(os.path.join(d, "src", "client.rs"), '''\
//! %s's OWN customer gateway client: GET /v1/customers/{id}.
//! The host comes from the service's own CUSTOMERS_BASE env var.

pub const BASE_ENV: &str = "CUSTOMERS_BASE";

pub fn customer_url(id: &str) -> String {
    let base = std::env::var(BASE_ENV)
        .unwrap_or_else(|_| "http://customers.internal".to_string());
    format!("{}/v1/customers/{}", base, id)
}

pub fn field_name() -> &'static str {
    "customer_id"
}
''' % name)
    return "test_artifacts.py", _artifact_test(name, [
        ("src/client.rs", "/v1/customers/"),
        ("src/client.rs", '"customer_id"'),
    ], "own HTTP client reads its own field")


# ── java shape sources (artifact tests) ─────────────────────────────────────

def _sh_java_dto(d, name):
    cls = _camel(name)
    _write(os.path.join(d, "src", "main", "java", "%s.java" % cls), '''\
import org.json.JSONObject;

/**
 * %s's own profile projection over its own JSON payload
 * (field customer_id). No external service involved.
 */
public class %s {
    public String customer(JSONObject data) {
        return data.getString("customer_id");
    }
}
''' % (name, cls))
    return "test_artifacts.py", _artifact_test(name, [
        ("src/main/java/%s.java" % cls, 'data.getString("customer_id")'),
        ("src/main/java/%s.java" % cls, "public class %s" % cls),
    ], "own DTO reads its own field")


def _sh_java_http(d, name):
    cls = _camel(name)
    _write(os.path.join(d, "src", "main", "java", "%s.java" % cls), '''\
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.Response;

import java.io.IOException;

/**
 * %s's OWN customer gateway caller: GET /v1/customers/{id}.
 * The host comes from the service's own CUSTOMERS_BASE env var.
 */
public class %s {
    private final OkHttpClient client = new OkHttpClient();

    public boolean exists(String id) throws IOException {
        String base = System.getenv()
                .getOrDefault("CUSTOMERS_BASE", "http://customers.internal");
        Request request = new Request.Builder()
                .url(base + "/v1/customers/" + id).build();
        try (Response response = client.newCall(request).execute()) {
            String body = response.body().string();
            return body.contains("customer_id");
        }
    }
}
''' % (name, cls))
    return "test_artifacts.py", _artifact_test(name, [
        ("src/main/java/%s.java" % cls, "/v1/customers/"),
        ("src/main/java/%s.java" % cls, '"customer_id"'),
    ], "own HTTP client reads its own field")


_SHAPE_WRITERS = {
    "db": _sh_db, "http": _sh_http, "event": _sh_event, "queue": _sh_queue,
    "dto": _sh_dto, "health": _sh_health, "metrics": _sh_metrics,
    "hom_invoice": _sh_hom_invoice, "hom_sync": _sh_hom_sync,
    "hom_summary": _sh_hom_summary,
    "ts_dto": _sh_ts_dto, "ts_http": _sh_ts_http, "ts_event": _sh_ts_event,
    "ts_proto": _sh_ts_proto, "ts_clean": _sh_ts_clean,
    "go_json": _sh_go_json, "go_http": _sh_go_http, "go_proto": _sh_go_proto,
    "rs_serde": _sh_rs_serde, "rs_event": _sh_rs_event, "rs_http": _sh_rs_http,
    "java_dto": _sh_java_dto, "java_http": _sh_java_http,
}


def _role_for(shape):
    roles = {
        "db": "NOISE — own DB column customer_id, zero network",
        "http": "NOISE — own /v1/customers route via CUSTOMERS_BASE, zero coupling to orders",
        "event": "NOISE — own Kafka topic, payload carries customer_id, zero coupling",
        "queue": "NOISE — own AMQP queue via pika, payload carries customer_id, zero coupling",
        "dto": "NOISE — own wire DTO with a customer_id field, zero network",
        "health": "NOISE — liveness probe only, no customer data",
        "metrics": "NOISE — counter registry only, no customer data",
        "hom_invoice": "NOISE HOMONYM — own build_invoice over its own charge lines, zero coupling",
        "hom_sync": "NOISE HOMONYM — own sync_customer over its own rows, zero coupling",
        "hom_summary": "NOISE HOMONYM — own summarize over its own rows, zero coupling",
        "ts_dto": "NOISE — own TS DTO with a customer_id field, zero coupling",
        "ts_http": "NOISE — own TS client for /v1/customers, zero coupling",
        "ts_event": "TS NOISE — own TS consumer of its own topic, zero coupling",
        "ts_proto": "NOISE — own proto with a customer_id field, zero relation to orders.proto",
        "ts_clean": "NOISE — small helpers only, no customer data",
        "go_json": "NOISE — own Go DTO with a customer_id json tag, zero coupling",
        "go_http": "NOISE — own Go client for /v1/customers, zero coupling",
        "go_proto": "NOISE — own Go proto consumer, zero relation to orders.proto",
        "rs_serde": "NOISE — own Rust model with a customer_id field, zero coupling",
        "rs_event": "NOISE — own Rust event payload, zero coupling",
        "rs_http": "NOISE — own Rust client for /v1/customers, zero coupling",
        "java_dto": "NOISE — own Java DTO reading customer_id, zero coupling",
        "java_http": "NOISE — own Java client for /v1/customers, zero coupling",
    }
    return roles[shape]


# ── noise generation ────────────────────────────────────────────────────────

def _write_noise(root, name, shape):
    d = _init_repo(os.path.join(root, name))
    test_name, test_body = _SHAPE_WRITERS[shape](d, name)
    _commit_all(d, "%s: base" % name, 0)
    _write(os.path.join(d, "tests", test_name), test_body)
    _commit_all(d, "tests: green oracle (always green)", 1)
    return d


def _extend_repos_yaml(root):
    """Extend the mesh-written repos.yaml: `repos:` lists all 80 repos
    (mesh first, then the noise); `services:` stays byte-identical to
    what the mesh writer wrote (only orders)."""
    path = os.path.join(root, "repos.yaml")
    with open(path) as f:
        body = f.read()
    head, sep, tail = body.rpartition("\nservices:\n")
    if not sep:
        raise SystemExit("scale build: mesh repos.yaml has no services: block")
    entries = []
    for name, shape in NOISE:
        entries.append(
            '  - id: %s\n'
            '    language: %s\n'
            '    role: "%s"\n'
            '    source:\n'
            '      type: workspace_dir\n'
            '      path: %s\n'
            % (name, _SHAPE_LANG[shape], _role_for(shape),
               os.path.join(root, name)))
    new = head + "\n" + "".join(entries) + "services:\n" + tail
    with open(path, "w") as f:
        f.write(new)
    return path


def _audit_noise(root):
    """Self-audit: no noise file references an orders-svc contract
    surface, and at least 40 noise repos carry customer_id."""
    cid_repos = 0
    for name, _shape in NOISE:
        repo = os.path.join(root, name)
        has_cid = False
        for dirpath, dirnames, filenames in os.walk(repo):
            dirnames[:] = [d for d in dirnames if d != ".git"]
            for fn in filenames:
                p = os.path.join(dirpath, fn)
                try:
                    with open(p, encoding="utf-8") as f:
                        text = f.read()
                except (OSError, UnicodeDecodeError):
                    continue
                for pat in FORBIDDEN_IN_NOISE:
                    if pat in text:
                        raise SystemExit(
                            "scale build: %s references forbidden %r"
                            % (p, pat))
                rel = os.path.relpath(p, repo)
                if not rel.startswith("tests" + os.sep) \
                        and "customer_id" in text:
                    has_cid = True
        if has_cid:
            cid_repos += 1
    if cid_repos < 40:
        raise SystemExit("scale build: only %d noise repos carry customer_id"
                         % cid_repos)
    return cid_repos


# ── build / apply-edit ──────────────────────────────────────────────────────

def build(dest):
    """Mesh first (its repos.yaml is the base), then the 70 noise repos,
    then extend repos: to all 80 entries."""
    build_mesh(dest)
    for name, shape in NOISE:
        _write_noise(dest, name, shape)
    _extend_repos_yaml(dest)
    _audit_noise(dest)
    return dest


def apply_edit(dest):
    """The scripted change is the mesh's — traps and noise untouched."""
    return apply_edit_mesh(dest)


def main(argv):
    if len(argv) != 3 or argv[1] not in ("build", "apply-edit"):
        print(__doc__, file=sys.stderr)
        return 2
    cmd, dest = argv[1], argv[2]
    if cmd == "build":
        if os.path.isdir(dest) and os.listdir(dest):
            print(f"refusing to build into non-empty directory: {dest}",
                  file=sys.stderr)
            return 1
        os.makedirs(dest, exist_ok=True)
        build(dest)
    else:
        apply_edit(dest)
    print(dest)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
