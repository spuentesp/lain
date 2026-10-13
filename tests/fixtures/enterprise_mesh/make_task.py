#!/usr/bin/env python3
"""Build the enterprise-mesh fixture — the coupling-class decisive experiment.

Ten deterministic git repos around ONE contract change: orders-svc renames
the field `customer_id` -> `customer_ref` (same type) across THREE contract
surfaces at once (gRPC proto field, OpenAPI response property, Kafka event
payload key) plus a legacy AMQP publish. The oracle (ground_truth.json,
declared BEFORE any query) is about blast-radius quality per coupling class:

- billing-svc (Python) calls orders' HTTP route with the host from
  os.environ['FIN_ORDERS_BASE'] — the env->host mapping lives ONLY in
  this fixture's repos.yaml services[].env (class A env-host)
- logistics-svc (Go) calls Orders.GetOrder via checked-in generated pb
  stubs, reads resp.CustomerId (class A gRPC stub)
- notify-worker (Python) consumes topic order.events (confluent_kafka),
  reads payload['customer_id'] (class A Kafka)
- analytics-svc (TS) consumes the same topic (kafkajs) but reads the
  field through a dynamic key payload['customer_' + kind] (class B)
- fraud-check (Java) calls a host from RISK_ORDERS_URL — deliberately
  NOT declared in repos.yaml (class B unbound host)
- ledger-svc (Python) consumes AMQP queue order-events via pika — no
  Lain sensor exists for AMQP (class C blind spot)
- legacy-crm (Python) syncs its own DB column customer_id — HOMONYM
  TRAP, zero network
- catalog-svc (Python) calls orders' route via its OWN env var but
  reads only `total` — NEAR-MISS TRAP
- support-portal (TS) gRPC consumer of its OWN tickets.proto, which
  also has a customer_id field — PROTO HOMONYM TRAP

Oracle tests (stdlib-only unittest; frameworks stubbed, contract
artifacts read as text where the language has no runtime here) live in
all ten repos — the seven coupled repos red before `apply-edit`, green
after; the three traps green throughout.

Usage:
    make_task.py build <dir>        # 10 repos + oracle tests + commits
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
"""Contract-artifact oracle for the change: orders-svc renames the wire
field `customer_id` -> `customer_ref` on every surface it owns — the
gRPC proto field, the OpenAPI response property, the Kafka payload key
and the legacy AMQP payload key.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _read(rel):
    with open(os.path.join(ROOT, rel)) as f:
        return f.read()


class SeedSurfacesUseCustomerRef(unittest.TestCase):
    def test_proto_field_is_renamed(self):
        text = _read("protos/orders.proto")
        self.assertIn("string customer_ref = 1;", text)
        self.assertNotIn("customer_id", text)

    def test_openapi_property_is_renamed(self):
        text = _read("openapi.yaml")
        self.assertIn("required: [customer_ref, total, status]", text)
        self.assertIn("\\n        customer_ref:\\n", text)
        self.assertNotIn("customer_id", text)

    def test_kafka_payload_key_is_renamed(self):
        text = _read("src/kafka.rs")
        self.assertIn(\'json!({"customer_ref":\', text)
        self.assertNotIn(\'json!({"customer_id":\', text)

    def test_amqp_payload_key_is_renamed(self):
        text = _read("src/legacy_amqp.rs")
        self.assertIn(\'json!({"customer_ref":\', text)
        self.assertNotIn(\'json!({"customer_id":\', text)


if __name__ == "__main__":
    unittest.main()
'''

BILLING_TEST = '''\
"""Behavioral oracle for the change: with orders-svc's NEW response
field name (it renamed customer_id -> customer_ref), build_invoice must
re-emit the customer value under that same new name. This is the CLASS
A env-host hop — its base URL comes from os.environ['FIN_ORDERS_BASE'],
declared under services[].env in repos.yaml and nowhere else.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("httpx", types.ModuleType("httpx"))

os.environ.setdefault("FIN_ORDERS_BASE", "http://orders.internal")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import main  # noqa: E402  (billing's service module)


class BuildInvoiceEmitsTheNewWireName(unittest.TestCase):
    def test_emits_the_new_wire_name(self):
        # orders-svc's response after the rename — the wire field
        # arrives under its new name; billing must follow it.
        main.fetch_order = lambda order_id: {"customer_ref": "C-1", "total": 10}
        invoice = main.build_invoice("ord-1")
        self.assertEqual(invoice.get("customer_ref"), "C-1")
        self.assertEqual(invoice.get("amount"), 10)


if __name__ == "__main__":
    unittest.main()
'''

LOGISTICS_TEST = '''\
"""Contract-artifact oracle for the change: the checked-in generated
gRPC stubs and the handler must carry the NEW proto field name
(CustomerRef, from customer_id -> customer_ref in orders.proto).

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _read(rel):
    with open(os.path.join(ROOT, rel)) as f:
        return f.read()


class GrpcStubCarriesTheRenamedField(unittest.TestCase):
    def test_stub_field_and_accessor_are_renamed(self):
        text = _read("pb/orders.pb.go")
        self.assertIn("CustomerRef", text)
        self.assertIn("GetCustomerRef", text)
        self.assertNotIn("CustomerId", text)

    def test_handler_reads_the_renamed_accessor(self):
        text = _read("src/handler.go")
        self.assertIn("resp.CustomerRef", text)
        self.assertNotIn("resp.CustomerId", text)


if __name__ == "__main__":
    unittest.main()
'''

NOTIFY_TEST = '''\
"""Behavioral oracle for the change: with orders-svc's NEW Kafka
payload key (customer_id -> customer_ref on topic order.events),
on_message must still put the customer value in the notice. CLASS A
Kafka hop — the topic binding is literal, the payload key is not.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

_stub = types.ModuleType("confluent_kafka")


class _Consumer:
    def __init__(self, config):
        self.config = config
        self.topics = []

    def subscribe(self, topics):
        self.topics = list(topics)


_stub.Consumer = _Consumer
sys.modules.setdefault("confluent_kafka", _stub)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import worker  # noqa: E402  (notify's worker module)


class OnMessageCarriesTheCustomerValue(unittest.TestCase):
    def test_notice_carries_the_value_under_the_new_key(self):
        # orders-svc's event after the rename — the value arrives
        # under the new wire key; the worker must read that key.
        notice = worker.on_message({"customer_ref": "C-1", "total": 10})
        self.assertIn("C-1", notice["body"])


if __name__ == "__main__":
    unittest.main()
'''

ANALYTICS_TEST = '''\
"""Contract-artifact oracle for the change: the consumer reads the
field through a dynamic key (`customer_' + kind`). After the rename
the default kind must be "ref" — nothing else in this repo pins the
full key, so this one constant is the whole coupling.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class DynamicKindDefaultsToRef(unittest.TestCase):
    def test_kind_defaults_to_the_new_suffix(self):
        with open(os.path.join(ROOT, "src", "handler.ts")) as f:
            text = f.read()
        self.assertIn(\'const kind = "ref";\', text)
        self.assertNotIn(\'const kind = "id";\', text)


if __name__ == "__main__":
    unittest.main()
'''

FRAUD_TEST = '''\
"""Contract-artifact oracle for the change: the OkHttp call reads the
order's customer field under the NEW wire name. The host comes from
RISK_ORDERS_URL, an env var deliberately absent from repos.yaml — the
call is real, its attribution is heuristic.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class CheckReadsTheRenamedField(unittest.TestCase):
    def test_reads_the_new_wire_name(self):
        with open(os.path.join(ROOT, "src", "main", "java",
                               "FraudCheck.java")) as f:
            text = f.read()
        self.assertIn(\'getString("customer_ref")\', text)
        self.assertNotIn(\'getString("customer_id")\', text)


if __name__ == "__main__":
    unittest.main()
'''

LEDGER_TEST = '''\
"""Behavioral oracle for the change: with the NEW payload key on the
order-events AMQP queue (customer_id -> customer_ref), handle_delivery
must still record the customer value. CLASS C blind bus — no Lain
sensor exists for AMQP; this repo is the honesty check.

These tests are the oracle for the task. They are expected to FAIL
before the change and PASS after it. Do not edit them.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("pika", types.ModuleType("pika"))

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
import ledger  # noqa: E402  (ledger's projection module)


class HandleDeliveryRecordsTheCustomerValue(unittest.TestCase):
    def test_ledger_records_the_value_under_the_new_key(self):
        # orders-svc's AMQP payload after the rename — the value
        # arrives under the new wire key; the ledger must follow.
        ledger.handle_delivery({"customer_ref": "C-1", "total": 10})
        self.assertEqual(ledger.RECORDS[-1]["customer_id"], "C-1")
        self.assertTrue(ledger.RECORDS[-1]["posted"])


if __name__ == "__main__":
    unittest.main()
'''

CRM_TEST = '''\
"""Standalone oracle: legacy-crm's sync_customer is built from its own
sqlite row and never touches any orders surface. This is the HOMONYM
TRAP — these tests are GREEN before AND after the change; editing this
repo is a clobbering / false positive.

These tests must PASS at all times. Do not edit them.
"""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "src"))
from crm import sync_customer  # noqa: E402


class CrmSyncsFromItsOwnRow(unittest.TestCase):
    def test_syncs_its_own_db_column(self):
        result = sync_customer({"customer_id": "C-9", "name": "Ada"})
        self.assertEqual(result["crm_id"], "C-9")
        self.assertTrue(result["synced"])


if __name__ == "__main__":
    unittest.main()
'''

CATALOG_TEST = '''\
"""Standalone oracle: catalog's summarize reads only `total` from
orders' endpoint (via its OWN env var CATALOG_ORDERS_BASE). This is
the NEAR-MISS TRAP — same route, different field; these tests are
GREEN before AND after the change; editing this repo is a false
positive.

These tests must PASS at all times. Do not edit them.
"""
import os
import sys
import types
import unittest

sys.modules.setdefault("httpx", types.ModuleType("httpx"))

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

TICKET_TEST = '''\
"""Standalone oracle: support-portal's show_ticket renders its OWN
tickets.proto message (which has its own customer_id field); no
external proto is involved. This is the PROTO HOMONYM TRAP — these
tests are GREEN before AND after the change; editing this repo is a
false positive.

These tests must PASS at all times. Do not edit them.
"""
import os
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class ShowTicketRendersItsOwnField(unittest.TestCase):
    def test_show_reads_the_tickets_field(self):
        with open(os.path.join(ROOT, "src", "show.ts")) as f:
            text = f.read()
        self.assertIn("t.customerId", text)


if __name__ == "__main__":
    unittest.main()
'''


def _git(dir_, *args, offset_days=0):
    env = dict(os.environ)
    stamp = f"{BASE_EPOCH + offset_days * 86400} +0000"
    env.update(
        GIT_AUTHOR_NAME="enterprise-mesh-fixture", GIT_AUTHOR_EMAIL="fixture@lain.local",
        GIT_COMMITTER_NAME="enterprise-mesh-fixture", GIT_COMMITTER_EMAIL="fixture@lain.local",
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
    d = _init_repo(os.path.join(root, "orders-svc"))
    _write(os.path.join(d, "Cargo.toml"), '''\
[package]
name = "orders-svc"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rdkafka = "0.36"
''')
    _write(os.path.join(d, "protos", "orders.proto"), '''\
syntax = "proto3";

package orders;

message GetOrderRequest {
  string id = 1;
}

message Order {
  string customer_id = 1;
  int64 total = 2;
  string status = 3;
}

service Orders {
  rpc GetOrder(GetOrderRequest) returns (Order);
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
  /v1/orders/{id}:
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
    _write(os.path.join(d, "amqp.toml"), '''\
[amqp]
url = "amqp://guest:guest@rabbitmq:5672/%2f"
queue = "order-events"
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
    _write(os.path.join(d, "src", "kafka.rs"), '''\
//! Kafka producer for the order.events topic (rdkafka).
use rdkafka::producer::{FutureProducer, FutureRecord};
use serde_json::json;

pub async fn publish_order_event(
    producer: &FutureProducer,
    order_id: &str,
    customer_id: &str,
    total: i64,
) {
    let payload = json!({"customer_id": customer_id, "total": total});
    let record = FutureRecord::to("order.events")
        .key(order_id)
        .payload(&payload.to_string());
    let _ = producer.send(record, 0).await;
}
''')
    _write(os.path.join(d, "src", "legacy_amqp.rs"), '''\
//! Legacy AMQP publisher kept for old consumers of the
//! order-events queue. The queue name is read from amqp.toml.
use serde_json::json;

pub fn load_queue_name(path: &str) -> String {
    let cfg = std::fs::read_to_string(path).expect("amqp.toml");
    for line in cfg.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("queue") {
            if let Some(q) = rest.split(\'"\').nth(1) {
                return q.to_string();
            }
        }
    }
    panic!("queue name missing from amqp.toml");
}

pub fn order_event(customer_id: &str, total: i64) -> String {
    json!({"customer_id": customer_id, "total": total}).to_string()
}
''')
    _write(os.path.join(d, "src", "main.rs"), '''\
mod kafka;
mod legacy_amqp;
mod models;

fn main() {
    let order = models::Order {
        customer_id: String::from("c1"),
        total: 10,
        status: String::from("open"),
    };
    println!("order {} total {}", order.customer_id, order.total);
}
''')
    _commit_all(d, "orders-svc: base", 0)
    return d


def _write_billing(root):
    d = _init_repo(os.path.join(root, "billing-svc"))
    _write(os.path.join(d, "requirements.txt"), '''\
httpx==0.27.0
''')
    _write(os.path.join(d, "src", "main.py"), '''\
"""Billing service — invoices built from orders fetched over HTTP.

The orders host comes from FIN_ORDERS_BASE; the env->service mapping
is declared in repos.yaml services[].env and nowhere else.
"""
import os
from typing import Any

import httpx

BASE = os.environ["FIN_ORDERS_BASE"]


def fetch_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(f"{BASE}/v1/orders/{order_id}")
    return r.json()


def build_invoice(order_id: str) -> dict[str, Any]:
    data = fetch_order(order_id)
    return {"customer_id": data["customer_id"], "amount": data["total"]}
''')
    _commit_all(d, "billing-svc: base", 0)
    return d


def _write_logistics(root):
    d = _init_repo(os.path.join(root, "logistics-svc"))
    _write(os.path.join(d, "go.mod"), '''\
module logistics-svc

go 1.22
''')
    _write(os.path.join(d, "pb", "orders.pb.go"), '''\
// Code generated by protoc-gen-go. DO NOT EDIT.
// source: orders.proto

package pb

type GetOrderRequest struct {
	Id string
}

type Order struct {
	CustomerId string
	Total      int64
	Status     string
}

func (x *Order) GetCustomerId() string {
	if x != nil {
		return x.CustomerId
	}
	return ""
}

func (x *Order) GetTotal() int64 {
	if x != nil {
		return x.Total
	}
	return 0
}

type OrdersClient interface {
	GetOrder(req *GetOrderRequest) (*Order, error)
}

type ordersClient struct {
	cc interface{}
}

func NewOrdersClient(cc interface{}) OrdersClient {
	return &ordersClient{cc}
}

func (c *ordersClient) GetOrder(req *GetOrderRequest) (*Order, error) {
	return &Order{}, nil
}
''')
    _write(os.path.join(d, "src", "handler.go"), '''\
// Package logistics calls orders-svc over gRPC — no HTTP anywhere.
// The dial target comes from ORDERS_GRPC_ADDR.
package logistics

import (
	"context"
	"os"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"logistics-svc/pb"
)

func fetchOrder(ctx context.Context) (*pb.Order, error) {
	target := os.Getenv("ORDERS_GRPC_ADDR")
	conn, err := grpc.NewClient(target, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return nil, err
	}
	ordersClient := pb.NewOrdersClient(conn)
	return ordersClient.GetOrder(ctx, &pb.GetOrderRequest{Id: "1"})
}

func GetOrder(ctx context.Context) (string, error) {
	resp, err := fetchOrder(ctx)
	if err != nil {
		return "", err
	}
	return resp.CustomerId, nil
}
''')
    _commit_all(d, "logistics-svc: base", 0)
    return d


def _write_notify(root):
    d = _init_repo(os.path.join(root, "notify-worker"))
    _write(os.path.join(d, "requirements.txt"), '''\
confluent-kafka==2.3.0
''')
    _write(os.path.join(d, "src", "worker.py"), '''\
"""Order-events worker — consumes Kafka topic order.events."""
from typing import Any

from confluent_kafka import Consumer

CONSUMER_CONFIG = {"group.id": "notify-worker", "bootstrap.servers": "kafka:9092"}


def build_consumer() -> Consumer:
    consumer = Consumer(CONSUMER_CONFIG)
    consumer.subscribe(["order.events"])
    return consumer


def on_message(payload: dict[str, Any]) -> dict[str, str]:
    return {
        "channel": "email",
        "body": "order for " + str(payload["customer_id"]),
    }
''')
    _commit_all(d, "notify-worker: base", 0)
    return d


def _write_analytics(root):
    d = _init_repo(os.path.join(root, "analytics-svc"))
    _write(os.path.join(d, "package.json"), '''\
{
  "name": "analytics-svc",
  "version": "0.1.0",
  "private": true,
  "dependencies": {
    "kafkajs": "^2.2.4"
  }
}
''')
    _write(os.path.join(d, "src", "handler.ts"), '''\
// Consumes order.events (kafkajs). The payload field suffix is
// assembled at runtime — nothing in this repo pins the full key.
import { Kafka } from "kafkajs";

const kafka = new Kafka({ clientId: "analytics-svc", brokers: ["kafka:9092"] });

export async function start(): Promise<void> {
  const consumer = kafka.consumer({ groupId: "analytics-svc" });
  await consumer.connect();
  await consumer.subscribe({ topics: ["order.events"] });
  await consumer.run({
    eachMessage: async ({ message }) => {
      const payload = JSON.parse(message.value?.toString() ?? "{}");
      handle(payload);
    },
  });
}

export function handle(payload: Record<string, unknown>) {
  const kind = "id";
  const value = payload["customer_" + kind];
  return { customer: value };
}
''')
    _commit_all(d, "analytics-svc: base", 0)
    return d


def _write_fraud(root):
    d = _init_repo(os.path.join(root, "fraud-check"))
    _write(os.path.join(d, "src", "main", "java", "FraudCheck.java"), '''\
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.Response;

import org.json.JSONObject;

import java.io.IOException;

/**
 * Fraud scoring — pulls orders from a host that comes from
 * RISK_ORDERS_URL, an env var that is deliberately NOT declared in
 * any service registry.
 */
public class FraudCheck {
    private final OkHttpClient client = new OkHttpClient();

    public boolean check(String id) throws IOException {
        String url = System.getenv("RISK_ORDERS_URL") + "/v1/orders/" + id;
        Request request = new Request.Builder().url(url).build();
        try (Response response = client.newCall(request).execute()) {
            JSONObject data = new JSONObject(response.body().string());
            String customer = data.getString("customer_id");
            return customer != null && !customer.isEmpty();
        }
    }
}
''')
    _commit_all(d, "fraud-check: base", 0)
    return d


def _write_ledger(root):
    d = _init_repo(os.path.join(root, "ledger-svc"))
    _write(os.path.join(d, "requirements.txt"), '''\
pika==1.3.2
''')
    _write(os.path.join(d, "src", "ledger.py"), '''\
"""Ledger projections from the order-events AMQP queue (pika).

No Kafka, no HTTP — the AMQP bus is this repo's only input.
"""
from typing import Any

QUEUE = "order-events"
RECORDS: list[dict[str, Any]] = []


def bind_queue(channel) -> None:
    channel.queue_bind(queue=QUEUE, exchange="orders", routing_key=QUEUE)


def handle_delivery(payload: dict[str, Any]) -> None:
    RECORDS.append({"customer_id": payload["customer_id"], "posted": True})
''')
    _commit_all(d, "ledger-svc: base", 0)
    return d


def _write_crm(root):
    d = _init_repo(os.path.join(root, "legacy-crm"))
    _write(os.path.join(d, "src", "crm.py"), '''\
"""Local CRM sync — works off its own sqlite rows, zero network.

Its own DB column is named customer_id; same name as the wire field
on orders-svc's surfaces, on purpose.
"""
from typing import Any


def sync_customer(row: dict[str, Any]) -> dict[str, Any]:
    return {"crm_id": row["customer_id"], "synced": True}
''')
    _commit_all(d, "legacy-crm: base", 0)
    return d


def _write_catalog(root):
    d = _init_repo(os.path.join(root, "catalog-svc"))
    _write(os.path.join(d, "requirements.txt"), '''\
httpx==0.27.0
''')
    _write(os.path.join(d, "src", "catalog.py"), '''\
"""Catalog summaries built from orders' route — but only the total.

The host comes from the catalog's OWN env var CATALOG_ORDERS_BASE
(same route as billing, different registry story).
"""
import os
from typing import Any

import httpx

ORDERS_BASE = os.environ.get("CATALOG_ORDERS_BASE", "http://orders.invalid")


def fetch_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(ORDERS_BASE + f"/v1/orders/{order_id}")
    return r.json()


def summarize(order_id: str) -> dict[str, Any]:
    resp = fetch_order(order_id)
    return {"total": resp["total"]}
''')
    _commit_all(d, "catalog-svc: base", 0)
    return d


def _write_support(root):
    d = _init_repo(os.path.join(root, "support-portal"))
    _write(os.path.join(d, "package.json"), '''\
{
  "name": "support-portal",
  "version": "0.1.0",
  "private": true
}
''')
    _write(os.path.join(d, "protos", "tickets.proto"), '''\
syntax = "proto3";

package tickets;

message Ticket {
  string customer_id = 1;
  string subject = 2;
}

service Tickets {
  rpc GetTicket(GetTicketRequest) returns (Ticket);
}

message GetTicketRequest {
  string id = 1;
}
''')
    _write(os.path.join(d, "pb", "tickets.pb.js"), '''\
// Code generated by protoc-gen-js. DO NOT EDIT.
// source: tickets.proto

function Ticket(data) {
  this.customerId = (data && data.customer_id) || "";
  this.subject = (data && data.subject) || "";
}

Ticket.prototype.getCustomerId = function () {
  return this.customerId;
};

module.exports = { Ticket };
''')
    _write(os.path.join(d, "src", "show.ts"), '''\
// Renders a ticket from its OWN tickets.proto message. The shared
// field name is a coincidence — no external proto is involved.
export function show_ticket(t: { customerId: string; subject: string }) {
  return "ticket " + t.customerId + ": " + t.subject;
}
''')
    _commit_all(d, "support-portal: base", 0)
    return d


def _write_repos_yaml(root):
    # Roles copied verbatim from ground_truth.json. Built line-by-line
    # (not one f-string) because several roles contain braces.
    lines = ["data_dir: %s\nrepos:\n" % os.path.join(root, ".lain-data")]
    # Neutral service-registry role lines only — the kind a real catalog
    # carries. Never describe coupling, protocols, env vars or field names
    # here: a roles column that explains the joins hands the agent the
    # analysis (lesson of 2026-10-11 — the first mesh runs were contaminated
    # by answer-key roles in this file).
    for rid, lang, role in (
            ("orders-svc", "rust",
             "order management service"),
            ("billing-svc", "python",
             "billing and invoicing"),
            ("logistics-svc", "go",
             "logistics routing and dispatch"),
            ("notify-worker", "python",
             "customer notification worker"),
            ("analytics-svc", "typescript",
             "product analytics pipeline"),
            ("fraud-check", "java",
             "fraud scoring service"),
            ("ledger-svc", "python",
             "financial ledger recording"),
            ("legacy-crm", "python",
             "legacy CRM synchronization"),
            ("catalog-svc", "python",
             "product catalog service"),
            ("support-portal", "typescript",
             "customer support portal")):
        lines.append("  - id: %s\n" % rid)
        lines.append("    language: %s\n" % lang)
        lines.append('    role: "%s"\n' % role)
        lines.append("    source:\n")
        lines.append("      type: workspace_dir\n")
        lines.append("      path: %s\n" % os.path.join(root, rid))
    # Contract-federation section: the class-A joins. The orders service
    # claims host orders.internal and the env names FIN_ORDERS_BASE
    # (billing-svc HTTP) and ORDERS_GRPC_ADDR (logistics-svc gRPC dial) —
    # those joins exist nowhere else. RISK_ORDERS_URL (fraud-check) and
    # CATALOG_ORDERS_BASE (catalog-svc) are deliberately absent.
    lines.append("services:\n")
    lines.append("  - name: orders\n")
    lines.append("    repo: orders-svc\n")
    lines.append("    paths: []\n")
    lines.append("    hosts: [orders.internal]\n")
    lines.append("    env: [FIN_ORDERS_BASE, ORDERS_GRPC_ADDR]\n")
    _write(os.path.join(root, "repos.yaml"), "".join(lines))


# ── build / apply-edit ──────────────────────────────────────────────────────

def build(dest):
    for fn in (_write_orders, _write_billing, _write_logistics,
               _write_notify, _write_analytics, _write_fraud,
               _write_ledger, _write_crm, _write_catalog, _write_support):
        fn(dest)
    _write_repos_yaml(dest)
    os.makedirs(os.path.join(dest, "consensus"), exist_ok=True)
    for repo, body, name, note in (
            ("orders-svc", ORDERS_TEST, "test_contract_artifact.py",
             "red until the rename lands"),
            ("billing-svc", BILLING_TEST, "test_invoice_contract.py",
             "red until the rename lands"),
            ("logistics-svc", LOGISTICS_TEST, "test_grpc_contract.py",
             "red until the rename lands"),
            ("notify-worker", NOTIFY_TEST, "test_notice_contract.py",
             "red until the rename lands"),
            ("analytics-svc", ANALYTICS_TEST, "test_dynamic_key.py",
             "red until the rename lands"),
            ("fraud-check", FRAUD_TEST, "test_fraud_contract.py",
             "red until the rename lands"),
            ("ledger-svc", LEDGER_TEST, "test_ledger_contract.py",
             "red until the rename lands"),
            ("legacy-crm", CRM_TEST, "test_crm_standalone.py",
             "green throughout — homonym trap"),
            ("catalog-svc", CATALOG_TEST, "test_catalog_total.py",
             "green throughout — near-miss trap"),
            ("support-portal", TICKET_TEST, "test_ticket_standalone.py",
             "green throughout — proto homonym trap")):
        _write(os.path.join(dest, repo, "tests", name), body)
        _commit_all(os.path.join(dest, repo),
                    f"tests: enterprise-mesh oracle ({note})", 1)
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
    """The scripted change: the seed renames the field on every surface
    it owns (proto field, openapi property, Kafka payload key, AMQP
    payload key), then the coupled consumers follow — billing\'s
    re-emit, the generated gRPC stub + handler read, notify\'s payload
    key, analytics\' dynamic suffix, fraud\'s getString, ledger\'s
    payload key. The traps (legacy-crm, catalog-svc, support-portal)
    and the external consumer (partner-portal) are deliberately
    untouched."""
    _replace(os.path.join(dest, "orders-svc", "protos", "orders.proto"),
             "string customer_id = 1;",
             "string customer_ref = 1;",
             "the Order message\'s customer_id field")
    _replace(os.path.join(dest, "orders-svc", "openapi.yaml"),
             "      required: [customer_id, total, status]\n"
             "      properties:\n        customer_id:\n",
             "      required: [customer_ref, total, status]\n"
             "      properties:\n        customer_ref:\n",
             "the openapi Order schema\'s customer_id property")
    _replace(os.path.join(dest, "orders-svc", "src", "kafka.rs"),
             'json!({"customer_id":', 'json!({"customer_ref":',
             "the Kafka payload key in kafka.rs")
    _replace(os.path.join(dest, "orders-svc", "src", "legacy_amqp.rs"),
             'json!({"customer_id":', 'json!({"customer_ref":',
             "the AMQP payload key in legacy_amqp.rs")
    _replace(os.path.join(dest, "billing-svc", "src", "main.py"),
             '{"customer_id": data["customer_id"]',
             '{"customer_ref": data["customer_ref"]',
             "the customer_id read/re-emit in build_invoice")
    _replace(os.path.join(dest, "logistics-svc", "pb", "orders.pb.go"),
             "CustomerId", "CustomerRef",
             "the CustomerId field/accessor in the generated stub")
    _replace(os.path.join(dest, "logistics-svc", "src", "handler.go"),
             "resp.CustomerId", "resp.CustomerRef",
             "the resp.CustomerId read in GetOrder")
    _replace(os.path.join(dest, "notify-worker", "src", "worker.py"),
             'payload["customer_id"]', 'payload["customer_ref"]',
             "the payload key read in on_message")
    _replace(os.path.join(dest, "analytics-svc", "src", "handler.ts"),
             'const kind = "id"', 'const kind = "ref"',
             "the dynamic-key suffix in handle")
    _replace(os.path.join(dest, "fraud-check", "src", "main", "java",
                          "FraudCheck.java"),
             'getString("customer_id")', 'getString("customer_ref")',
             "the customer_id read in check")
    _replace(os.path.join(dest, "ledger-svc", "src", "ledger.py"),
             'payload["customer_id"]', 'payload["customer_ref"]',
             "the payload key read in handle_delivery")
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
