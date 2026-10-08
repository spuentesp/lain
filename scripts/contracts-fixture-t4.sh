#!/usr/bin/env bash
# Builds the Task 2 four-repo non-HTTP fixture
# (docs/superpowers/plans/2026-10-07-contract-soundness.md):
#
#   1. runs `scripts/contracts-fixture.sh <dir>` — the T1 fixture is
#      produced byte-for-byte unchanged, so every T1 scenario tag,
#      commit sha, and ground-truth metric stays identical;
#   2. adds exactly one non-HTTP service (plus the consumer sites
#      that make each path queryable) per repo, committed on each
#      repo's `main` (== `base`) with the same fixed
#      GIT_AUTHOR_DATE / GIT_COMMITTER_DATE identity as T1 and
#      tagged `t4`;
#   3. patches <dir>/repos.yaml with the `hosts` entries the gRPC
#      channel resolver and the WebSocket dial resolver need.
#
# Layout added on top of T1 (each file lands in the `t4` tag only;
# `base` and every scenario tag are untouched):
#
#   <dir>/orders/proto/orders.proto        gRPC provider (rpc GetOrder)
#   <dir>/orders/schema.graphql            GraphQL provider (Query.orders)
#   <dir>/billing/src/orders_rpc.py        gRPC consumer stub call
#   <dir>/billing/queries/orders.graphql   GraphQL consumer document
#   <dir>/billing/src/feed.py              WebSocket consumer (binds /feed)
#   <dir>/billing/src/market_feed.py       WebSocket consumer (unplaceable)
#   <dir>/reports/src/index.ts             + app.ws("/feed") provider
#   <dir>/platform/scripts/report.py       SQL reader (table:shipments)
#   <dir>/repos.yaml                       + hosts: [orders] / [reports]
#
# Why a sibling script instead of extending the T1 fixture in place:
# T1's ground truth (`tests/fixtures/contracts/ground_truth.yaml`,
# `pr13_hermetic_precision_recall_over_t1_fixture`) pins the base
# graph — extra joins over T1's base move `reads_field_precision`
# off 1.0. Keeping T1 pristine is a plan-level constraint (Global
# Constraints: T1 must stay at precision AND recall 1.0).
#
# Usage: scripts/contracts-fixture-t4.sh <dir>
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <dir>" >&2
  exit 2
fi

ROOT="$1"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Step 1 — the T1 fixture, unchanged.
"$SCRIPT_DIR/contracts-fixture.sh" "$ROOT"

# Deterministic commit identity + base epoch, identical to
# contracts-fixture.sh so the t4 commits are stable across runs.
GIT_AUTHOR_NAME="Lain Fixture"
GIT_AUTHOR_EMAIL="fixture@lain.local"
GIT_COMMITTER_NAME="Lain Fixture"
GIT_COMMITTER_EMAIL="fixture@lain.local"
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL
BASE_EPOCH=1736140800

# make_commit <message> <offset_days> — same shape as T1's.
make_commit() {
  local msg="$1"
  local offset="${2:-0}"
  local dt
  dt="$(date -u -d "@$((BASE_EPOCH + offset * 86400))" +%Y-%m-%dT%H:%M:%SZ)"
  git add -A
  GIT_AUTHOR_DATE="$dt" GIT_COMMITTER_DATE="$dt" \
    git -c user.name="$GIT_AUTHOR_NAME" -c user.email="$GIT_AUTHOR_EMAIL" \
    commit -qm "$msg"
}

# Offset 30: past every T1 scenario (max 22) so the t4 commit sorts
# after them in any date-ordered view of the repo.
T4_OFFSET=30

ORDERS_DIR="$ROOT/orders"
BILLING_DIR="$ROOT/billing"
REPORTS_DIR="$ROOT/reports"
PLATFORM_DIR="$ROOT/platform"

# ─── orders: gRPC provider + GraphQL provider ─────────────────────────
#
# `proto/orders.proto` declares one service with
# `rpc GetOrder (Order) returns (Order)`; grpc_provider_sensor mints
# an RpcProvider whose wire key is `rpc:orders/GetOrder`, and the
# request/response messages feed the endpoint schema so
# `get_contract` can render fields. The service is deliberately named
# `orders` so billing's `ordersStub` receiver (the consumer sensor
# strips the `Stub` suffix) keys to the same (service, method) pair.
#
# `schema.graphql` is billing's GraphQL target: its `Query.orders`
# root field is the `graphql:query:orders` provider, and the `Order`
# object type supplies the response fields (`id`, `customer_id`) the
# consumer's selected fields join against. The plan's fixture table
# lists the GraphQL provider column as "—", but `trace_impact` seeds
# need the endpoint to exist and consumers skip same-service
# providers, so it must live in orders (the consumer is in billing).
write_orders_t4() {
  cd "$ORDERS_DIR"
  mkdir -p proto
  cat > proto/orders.proto <<'EOF'
syntax = "proto3";

// T4 fixture: the request/response both ways. Fields become the
// endpoint's schema via the sensor's RequestSchema/ResponseSchema
// edges, which is what makes `rpc:orders/GetOrder` schema-backed.
message Order {
  string id = 1;
  string customer_id = 2;
  int64 total = 3;
}

service orders {
  rpc GetOrder (Order) returns (Order);
}
EOF
  cat > schema.graphql <<'EOF'
# T4 fixture: SDL provider for the orders service. graphql_provider_sensor
# emits one GraphqlProvider per root field on `type Query`; object types
# become Schema/Field nodes behind the ResponseSchema edge.
type Query {
  orders: [Order!]!
}

type Order {
  id: ID!
  customer_id: String!
}
EOF
  make_commit "t4: gRPC + GraphQL providers" "$T4_OFFSET"
  git tag t4
  git checkout -q main
}

# ─── billing: one consumer site per non-HTTP protocol ─────────────────
#
# Comments inside the generated files are written to avoid the
# sensors' own trigger patterns (several detectors scan raw lines
# without stripping comments); keep them comment-shaped.
write_billing_t4() {
  cd "$BILLING_DIR"
  mkdir -p queries
  cat > src/orders_rpc.py <<'EOF'
"""gRPC stub call into the orders service."""
import grpc

import orders_pb2


def fetch_order(order_id: str):
    # The host literal in the channel constructor below is the
    # joiner's resolution input (repos.yaml: hosts [orders]).
    channel = grpc.insecure_channel("orders:50051")
    ordersStub = orders_pb2.ordersStub(channel)
    return ordersStub.GetOrder(orders_pb2.Order(id=order_id))
EOF
  cat > queries/orders.graphql <<'EOF'
# T4 fixture: billing's read of orders' Query.orders. The provider
# sensor ignores this file (no `type Query` block); the consumer
# sensor emits one GraphqlConsumer + FieldRefs for id/customer_id.
query {
  orders {
    id
    customer_id
  }
}
EOF
  cat > src/feed.py <<'EOF'
"""Live order feed: dials the reports service's WebSocket endpoint."""


def open_feed():
    # ws://<host>/<route> — the websocket sensor takes the URL
    # literal; host + route are what the joiner matches on.
    return connect("ws://reports/feed")
EOF
  cat > src/market_feed.py <<'EOF'
"""Third-party price feed: a dial outside the federation."""


def open_market_feed():
    # Literal host that matches no service — list_unresolved must
    # surface this, not silence.
    return connect("wss://feeds.example.com/prices")
EOF
  # grpcio / websocket-client: the stub and the dials above are real
  # imports; keep requirements.txt coherent with the code.
  sed -i 's/^aiokafka==0.10.0$/aiokafka==0.10.0\ngrpcio==1.60.0\nwebsocket-client==1.7.0/' requirements.txt
  make_commit "t4: gRPC/GraphQL/WebSocket consumer sites" "$T4_OFFSET"
  git tag t4
  git checkout -q main
}

# ─── reports: WebSocket provider ──────────────────────────────────────
#
# Full-file rewrite of src/index.ts: T1's version plus the
# `app.ws("/feed", …)` registration. Keep the T1 body in sync with
# `write_reports_main` in contracts-fixture.sh if it ever changes.
write_reports_t4() {
  cd "$REPORTS_DIR"
  cat > src/index.ts <<'EOF'
import express from "express";
import cron from "node-cron";

const app = express();
const PORT = 3001;

async function buildMonthlyReport(id: string): Promise<unknown> {
  const res = await fetch(`${process.env.BILLING_URL}/invoices/${id}`);
  const invoice = await res.json();
  return invoice;
}

async function getMonthlyReport(_req: express.Request, res: express.Response): Promise<void> {
  const report = await buildMonthlyReport("inv-1");
  res.json(report);
}

async function scheduledMonthlyReport(): Promise<void> {
  await buildMonthlyReport("inv-1");
}

app.get("/reports/monthly", getMonthlyReport);

// T4 fixture (Task 2): WebSocket provider. The websocket sensor's
// server-route idiom (a quoted route argument on a ws registration)
// mints a WebSocketProvider whose wire key is websocket:/feed;
// billing's ws://reports/feed dial binds to it through reports'
// hosts entry.
function handleFeed(socket: { send: (data: string) => void }): void {
  socket.send(JSON.stringify({ type: "tick" }));
}

app.ws("/feed", handleFeed);

cron.schedule("0 0 1 * *", scheduledMonthlyReport);

app.listen(PORT);
EOF
  make_commit "t4: WebSocket /feed provider" "$T4_OFFSET"
  git tag t4
  git checkout -q main
}

# ─── platform: SQL reader ─────────────────────────────────────────────
#
# `cursor.execute("SELECT id FROM shipments")` — sql_sensor mints a
# Table node (wire key `table:shipments`) plus a ReadsTable edge.
# The file deliberately sits OUTSIDE services/{shipping,inventory}/
# so the service assignment falls back to the repo id: the endpoint
# is keyed platform / table:shipments, which is what get_contract
# must answer.
write_platform_t4() {
  cd "$PLATFORM_DIR"
  mkdir -p scripts
  cat > scripts/report.py <<'EOF'
"""Cross-service shipment report — a SQL read with no schema source."""


def load_shipments(cursor):
    # The SELECT below is the only schema source the sql sensor sees;
    # no DDL exists, so the endpoint is schemaless by construction.
    cursor.execute("SELECT id FROM shipments")
    return cursor.fetchall()
EOF
  make_commit "t4: SQL shipments read" "$T4_OFFSET"
  git tag t4
  git checkout -q main
}

# ─── repos.yaml hosts entries ─────────────────────────────────────────
#
# The gRPC channel resolver (`grpc.insecure_channel("orders:50051")`
# -> host "orders") and the WebSocket dial resolver
# ("ws://reports/feed" -> host "reports") only place a consumer when
# the target host matches some service's `hosts` entry. Patch the
# T1-generated file; unique (config Rule 7) and lower-case
# (config Rule 10) by construction.
patch_repos_yaml_hosts() {
  python3 - "$ROOT/repos.yaml" <<'PYEOF'
import sys

path = sys.argv[1]
with open(path) as f:
    lines = f.readlines()

out = []
in_reports = False
for line in lines:
    out.append(line)
    stripped = line.strip()
    if stripped == "env: [ORDERS_URL]":
        out.append("    hosts: [orders]\n")
    if stripped == "- name: reports":
        in_reports = True
    elif in_reports and stripped == "paths: []":
        out.append("    hosts: [reports]\n")
        in_reports = False

with open(path, "w") as f:
    f.writelines(out)
PYEOF
}

# ─── main ─────────────────────────────────────────────────────────────

write_orders_t4
write_billing_t4
write_reports_t4
write_platform_t4
patch_repos_yaml_hosts

{
  echo "fixture (t4): $ROOT"
  echo "orders t4:  $(git -C "$ORDERS_DIR" rev-parse t4)"
  echo "billing t4: $(git -C "$BILLING_DIR" rev-parse t4)"
  echo "reports t4: $(git -C "$REPORTS_DIR" rev-parse t4)"
  echo "platform t4: $(git -C "$PLATFORM_DIR" rev-parse t4)"
} >&2
