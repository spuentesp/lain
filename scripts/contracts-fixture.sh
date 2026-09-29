#!/usr/bin/env bash
# Builds the LAIN 0.9 contract federation fixture: four local git
# repositories (orders, billing, reports, platform) with scripted
# history and scenario tags, plus a <dir>/repos.yaml configured with
# `workspace_dir` sources and a `services` section. No network access.
# Fully deterministic: every commit uses fixed GIT_AUTHOR_DATE and
# GIT_COMMITTER_DATE values so the resulting refs and tree bytes are
# stable across runs.
#
# Layout written under <dir>:
#   <dir>/orders/         Rust (axum) + openapi.yaml (OAS 3.0)
#                         tags: base, s1-remove-customer-id, s2-add-currency,
#                         s5-enum-value, s6-rename-path, s11-rename-field,
#                         s12-rename-retype, s19-optional-request-type,
#                         s21-code-only-handler
#   <dir>/billing/        Python (FastAPI, httpx)
#                         tags: base, s3-dynamic-url, s5b-read-status,
#                         s20-read-discount, s22-cache-response
#   <dir>/reports/        TypeScript (Express, node-cron, fetch)
#                         tag: base
#   <dir>/platform/       Python monorepo (services/shipping, services/inventory)
#                         tag: base
#   <dir>/repos.yaml      workspace_dir sources and services
#
# Each scenario tag is a commit whose parent is `base` (detached HEAD),
# so `diff(base, <tag>)` reflects only the scenario's delta. Reports
# and platform ship only `base` because they back no scenario tag in
# §15.1.
#
# Usage: scripts/contracts-fixture.sh <dir>
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <dir>" >&2
  exit 2
fi

ROOT="$1"

# Deterministic commit identity. Exported so every `git commit` below
# inherits them regardless of whether the caller already has a global
# git config that would otherwise leak in.
GIT_AUTHOR_NAME="Lain Fixture"
GIT_AUTHOR_EMAIL="fixture@lain.local"
GIT_COMMITTER_NAME="Lain Fixture"
GIT_COMMITTER_EMAIL="fixture@lain.local"
export GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL

# Base epoch: 2025-01-06T09:00:00Z (a Monday). Each scenario commits
# at this base + N days, where N matches the scenario number where
# possible. Keeps history sortable without needing `git rebase`.
BASE_EPOCH=1736140800

# make_commit <message> <offset_days>
# Stage everything in the current directory and commit with fixed
# author/committer dates so the resulting object hash is stable.
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

ORDERS_DIR="$ROOT/orders"
BILLING_DIR="$ROOT/billing"
REPORTS_DIR="$ROOT/reports"
PLATFORM_DIR="$ROOT/platform"

# --- orders repo --------------------------------------------------------
#
# Rust (axum) + OAS 3.0 openapi.yaml. Four routes at base:
#   GET  /api/orders/{}        handler get_order        (code + openapi)
#   GET  /api/orders/me        handler get_me           (code + openapi)
#   POST /api/orders           handler create_order     (code + openapi)
#   GET  /api/orders/{}/label  handler get_order_label  (code only)
#
# Response shape: customer_id (string, req), total (integer, req),
# status (string, enum open|paid, req), items[].sku (string, req).
# Request body for POST: customer_id (req), items[].sku (req),
# note (optional).

write_orders_cargo() {
  cat > "$ORDERS_DIR/Cargo.toml" <<'EOF'
[package]
name = "orders"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = "0.7"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
EOF
}

write_orders_main_base() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_base() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customer_id, total, status, items]
      properties:
        customer_id:
          type: string
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s1-remove-customer-id: drop customer_id from response struct + schema.
write_orders_main_s1() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s1() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [total, status, items]
      properties:
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s2-add-currency: append optional `currency` to the response struct + schema.
write_orders_main_s2() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
    currency: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
        currency: Some("USD".to_string()),
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
        currency: Some("USD".to_string()),
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
        currency: Some("USD".to_string()),
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s2() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
                $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customer_id, total, status, items]
      properties:
        customer_id:
          type: string
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
        currency:
          type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s5-enum-value: add `Refunded` variant + `refunded` enum value.
write_orders_main_s5() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
    Refunded,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s5() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customer_id, total, status, items]
      properties:
        customer_id:
          type: string
        total:
          type: integer
        status:
          type: string
          enum: [open, paid, refunded]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s6-rename-path: /api/orders/{} → /api/order/{} (handler unchanged).
write_orders_main_s6() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/order/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s6() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
openapi: 3.0.0
info:
  title: Orders API
  version: 1.0.0
servers:
  - url: /
paths:
  /api/order/{id}:
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customer_id, total, status, items]
      properties:
        customer_id:
          type: string
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s11-rename-field: customer_id → customerId (same type).
write_orders_main_s11() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    #[serde(rename = "customerId")]
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s11() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customerId, total, status, items]
      properties:
        customerId:
          type: string
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s12-rename-retype: customer_id → customerId AND type string → integer.
write_orders_main_s12() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    #[serde(rename = "customerId")]
    customer_id: i64,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: 42,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: 0,
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: 0,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s12() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customerId, total, status, items]
      properties:
        customerId:
          type: integer
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: string
EOF
}

# s19-optional-request-type: optional request field `note` type change.
write_orders_main_s19() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<i64>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s19() {
  cat > "$ORDERS_DIR/openapi.yaml" <<'EOF'
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
  /api/orders/me:
    get:
      operationId: getMe
      responses:
        "200":
          description: The current user's order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
  /api/orders:
    post:
      operationId: createOrder
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/CreateOrder"
      responses:
        "200":
          description: The created order
          content:
            application/json:
              schema:
                $ref: "#/components/schemas/Order"
components:
  schemas:
    Order:
      type: object
      required: [customer_id, total, status, items]
      properties:
        customer_id:
          type: string
        total:
          type: integer
        status:
          type: string
          enum: [open, paid]
        items:
          type: array
          items:
            type: object
            required: [sku]
            properties:
              sku:
                type: string
    OrderItem:
      type: object
      required: [sku]
      properties:
        sku:
          type: string
    CreateOrder:
      type: object
      required: [customer_id, items]
      properties:
        customer_id:
          type: string
        items:
          type: array
          items:
            $ref: "#/components/schemas/OrderItem"
        note:
          type: integer
EOF
}

# s21-code-only-handler: change body of get_order_label (code-only route,
# no schema). Triggers ChangedWithoutSchema because a source file differs
# between base and head.
write_orders_main_s21() {
  cat > "$ORDERS_DIR/src/main.rs" <<'EOF'
use axum::{
    extract::Path,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
struct Order {
    customer_id: String,
    total: i64,
    status: OrderStatus,
    items: Vec<OrderItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum OrderStatus {
    Open,
    Paid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderItem {
    sku: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CreateOrder {
    customer_id: String,
    items: Vec<OrderItem>,
    note: Option<String>,
}

async fn get_order(Path(id): Path<String>) -> Json<Order> {
    Json(Order {
        customer_id: id,
        total: 100,
        status: OrderStatus::Open,
        items: vec![OrderItem { sku: "sku-1".to_string() }],
    })
}

async fn get_me() -> Json<Order> {
    Json(Order {
        customer_id: "me".to_string(),
        total: 0,
        status: OrderStatus::Open,
        items: vec![],
    })
}

async fn create_order(Json(body): Json<CreateOrder>) -> Json<Order> {
    Json(Order {
        customer_id: body.customer_id,
        total: 100,
        status: OrderStatus::Open,
        items: body.items,
    })
}

async fn get_order_label(Path(id): Path<String>) -> String {
    format!("label-{id}-v2")
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/api/orders/:id", get(get_order))
        .route("/api/orders/me", get(get_me))
        .route("/api/orders", post(create_order))
        .route("/api/orders/:id/label", get(get_order_label));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
EOF
}

write_orders_openapi_s21() {
  # OpenAPI unchanged from base; s21 only mutates the handler body.
  write_orders_openapi_base
}

write_orders() {
  mkdir -p "$ORDERS_DIR/src"
  cd "$ORDERS_DIR"
  git init -q -b main
  git config user.name "$GIT_AUTHOR_NAME"
  git config user.email "$GIT_AUTHOR_EMAIL"

  write_orders_cargo
  write_orders_main_base
  write_orders_openapi_base
  make_commit "orders: base" 0
  git tag base

  # Each scenario is a detached-HEAD commit whose parent is `base`,
  # so `diff(base, <tag>)` reflects only the scenario's delta. The
  # pattern is: `git checkout base`, write scenario files, `make_commit`,
  # then `git tag` — the tag points at the new commit.

  git checkout -q base
  write_orders_main_s1
  write_orders_openapi_s1
  make_commit "s1-remove-customer-id" 1
  git tag "s1-remove-customer-id"

  git checkout -q base
  write_orders_main_s2
  write_orders_openapi_s2
  make_commit "s2-add-currency" 2
  git tag "s2-add-currency"

  git checkout -q base
  write_orders_main_s5
  write_orders_openapi_s5
  make_commit "s5-enum-value" 5
  git tag "s5-enum-value"

  git checkout -q base
  write_orders_main_s6
  write_orders_openapi_s6
  make_commit "s6-rename-path" 6
  git tag "s6-rename-path"

  git checkout -q base
  write_orders_main_s11
  write_orders_openapi_s11
  make_commit "s11-rename-field" 11
  git tag "s11-rename-field"

  git checkout -q base
  write_orders_main_s12
  write_orders_openapi_s12
  make_commit "s12-rename-retype" 12
  git tag "s12-rename-retype"

  git checkout -q base
  write_orders_main_s19
  write_orders_openapi_s19
  make_commit "s19-optional-request-type" 19
  git tag "s19-optional-request-type"

  git checkout -q base
  write_orders_main_s21
  write_orders_openapi_s21
  make_commit "s21-code-only-handler" 21
  git tag "s21-code-only-handler"

  git checkout -q main
}

# --- billing repo --------------------------------------------------------
#
# Python (FastAPI, httpx). Module-level `ORDERS_URL = os.environ["ORDERS_URL"]`.
# `fetch_order` does an httpx.get against `f"{ORDERS_URL}/api/orders/{id}"`.
# `build_invoice` calls `fetch_order` and reads `customer_id` and `total`.
# `get_invoice` (GET /invoices/{}) calls `build_invoice`. `fetch_me`
# calls `/api/orders/me`. `print_label` calls `/api/orders/{id}/label`.
# `charge` calls `https://api.stripe.com/v1/charges` (external host).

write_billing_main_base() {
  cat > "$BILLING_DIR/src/main.py" <<'EOF'
"""Billing service: builds invoices from orders, charges customers."""
import os
from typing import Any

import httpx
from fastapi import FastAPI
from pydantic import BaseModel

ORDERS_URL = os.environ["ORDERS_URL"]

app = FastAPI()


class Invoice(BaseModel):
    id: str
    customer_id: str
    total: int


def fetch_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}")
    return r.json()


def build_invoice(order_id: str) -> Invoice:
    order = fetch_order(order_id)
    me = fetch_me()
    print_label(order_id)
    return Invoice(
        id=order_id,
        customer_id=order["customer_id"],
        total=order["total"],
    )


def fetch_me() -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/me")
    return r.json()


def print_label(order_id: str) -> str:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}/label")
    return r.text


def charge(order_id: str) -> dict[str, Any]:
    r = httpx.post("https://api.stripe.com/v1/charges", json={"order_id": order_id})
    return r.json()


@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: str) -> Invoice:
    return build_invoice(invoice_id)
EOF
}

write_billing_requirements() {
  cat > "$BILLING_DIR/requirements.txt" <<'EOF'
fastapi==0.110.0
httpx==0.27.0
pydantic==2.6.0
uvicorn==0.29.0
EOF
}

# s3-dynamic-url: replace fetch_order with a version whose URL is built
# from an unmapped function call. Path is `/v1/api/orders/{}` so plain
# matching against orders's `/api/orders/{}` fails, but stripping
# `/v1/` via prefix tolerance matches — list_unresolved lists it with
# the orders endpoint as candidate.
write_billing_main_s3() {
  cat > "$BILLING_DIR/src/main.py" <<'EOF'
"""Billing service: builds invoices from orders, charges customers."""
import os
from typing import Any

import httpx
from fastapi import FastAPI
from pydantic import BaseModel

ORDERS_URL = os.environ["ORDERS_URL"]

app = FastAPI()


class Invoice(BaseModel):
    id: str
    customer_id: str
    total: int


def fetch_order(order_id: str) -> dict[str, Any]:
    base = compute_base()
    r = httpx.get(f"{base}/v1/api/orders/{order_id}")
    return r.json()


def compute_base() -> str:
    return ORDERS_URL


def build_invoice(order_id: str) -> Invoice:
    order = fetch_order(order_id)
    me = fetch_me()
    print_label(order_id)
    return Invoice(
        id=order_id,
        customer_id=order["customer_id"],
        total=order["total"],
    )


def fetch_me() -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/me")
    return r.json()


def print_label(order_id: str) -> str:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}/label")
    return r.text


def charge(order_id: str) -> dict[str, Any]:
    r = httpx.post("https://api.stripe.com/v1/charges", json={"order_id": order_id})
    return r.json()


@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: str) -> Invoice:
    return build_invoice(invoice_id)
EOF
}

# s5b-read-status: build_invoice also reads `status` from the order.
write_billing_main_s5b() {
  cat > "$BILLING_DIR/src/main.py" <<'EOF'
"""Billing service: builds invoices from orders, charges customers."""
import os
from typing import Any

import httpx
from fastapi import FastAPI
from pydantic import BaseModel

ORDERS_URL = os.environ["ORDERS_URL"]

app = FastAPI()


class Invoice(BaseModel):
    id: str
    customer_id: str
    total: int
    status: str


def fetch_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}")
    return r.json()


def build_invoice(order_id: str) -> Invoice:
    order = fetch_order(order_id)
    me = fetch_me()
    print_label(order_id)
    return Invoice(
        id=order_id,
        customer_id=order["customer_id"],
        total=order["total"],
        status=order["status"],
    )


def fetch_me() -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/me")
    return r.json()


def print_label(order_id: str) -> str:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}/label")
    return r.text


def charge(order_id: str) -> dict[str, Any]:
    r = httpx.post("https://api.stripe.com/v1/charges", json={"order_id": order_id})
    return r.json()


@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: str) -> Invoice:
    return build_invoice(invoice_id)
EOF
}

# s20-read-discount: build_invoice also reads `discount` from the order,
# a field that does not exist in the orders schema. The read emits a
# FieldRef that fails field join → ConsumerFieldUnmatched.
write_billing_main_s20() {
  cat > "$BILLING_DIR/src/main.py" <<'EOF'
"""Billing service: builds invoices from orders, charges customers."""
import os
from typing import Any

import httpx
from fastapi import FastAPI
from pydantic import BaseModel

ORDERS_URL = os.environ["ORDERS_URL"]

app = FastAPI()


class Invoice(BaseModel):
    id: str
    customer_id: str
    total: int
    discount: int


def fetch_order(order_id: str) -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}")
    return r.json()


def build_invoice(order_id: str) -> Invoice:
    order = fetch_order(order_id)
    me = fetch_me()
    print_label(order_id)
    return Invoice(
        id=order_id,
        customer_id=order["customer_id"],
        total=order["total"],
        discount=order["discount"],
    )


def fetch_me() -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/me")
    return r.json()


def print_label(order_id: str) -> str:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}/label")
    return r.text


def charge(order_id: str) -> dict[str, Any]:
    r = httpx.post("https://api.stripe.com/v1/charges", json={"order_id": order_id})
    return r.json()


@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: str) -> Invoice:
    return build_invoice(invoice_id)
EOF
}

# s22-cache-response: module-level `_CACHE` stores fetched responses.
# Assigning a bound identifier into a global collection is an escape
# (§6.5), so `reads_complete` becomes `false` on the consumer fact.
write_billing_main_s22() {
  cat > "$BILLING_DIR/src/main.py" <<'EOF'
"""Billing service: builds invoices from orders, charges customers."""
import os
from typing import Any

import httpx
from fastapi import FastAPI
from pydantic import BaseModel

ORDERS_URL = os.environ["ORDERS_URL"]

app = FastAPI()

_CACHE: dict[str, dict[str, Any]] = {}


class Invoice(BaseModel):
    id: str
    customer_id: str
    total: int


def fetch_order(order_id: str) -> dict[str, Any]:
    if order_id in _CACHE:
        return _CACHE[order_id]
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}")
    _CACHE[order_id] = r.json()
    return _CACHE[order_id]


def build_invoice(order_id: str) -> Invoice:
    order = fetch_order(order_id)
    me = fetch_me()
    print_label(order_id)
    return Invoice(
        id=order_id,
        customer_id=order["customer_id"],
        total=order["total"],
    )


def fetch_me() -> dict[str, Any]:
    r = httpx.get(f"{ORDERS_URL}/api/orders/me")
    return r.json()


def print_label(order_id: str) -> str:
    r = httpx.get(f"{ORDERS_URL}/api/orders/{order_id}/label")
    return r.text


def charge(order_id: str) -> dict[str, Any]:
    r = httpx.post("https://api.stripe.com/v1/charges", json={"order_id": order_id})
    return r.json()


@app.get("/invoices/{invoice_id}")
def get_invoice(invoice_id: str) -> Invoice:
    return build_invoice(invoice_id)
EOF
}

write_billing() {
  mkdir -p "$BILLING_DIR/src"
  cd "$BILLING_DIR"
  git init -q -b main
  git config user.name "$GIT_AUTHOR_NAME"
  git config user.email "$GIT_AUTHOR_EMAIL"

  write_billing_requirements
  write_billing_main_base
  make_commit "billing: base" 0
  git tag base

  git checkout -q base
  write_billing_main_s3
  make_commit "s3-dynamic-url" 3
  git tag "s3-dynamic-url"

  git checkout -q base
  write_billing_main_s5b
  make_commit "s5b-read-status" 5
  git tag "s5b-read-status"

  git checkout -q base
  write_billing_main_s20
  make_commit "s20-read-discount" 20
  git tag "s20-read-discount"

  git checkout -q base
  write_billing_main_s22
  make_commit "s22-cache-response" 22
  git tag "s22-cache-response"

  git checkout -q main
}

# --- reports repo --------------------------------------------------------
#
# TypeScript (Express, node-cron, fetch). `buildMonthlyReport` calls
# `fetch(`${process.env.BILLING_URL}/invoices/${id}`)` and is reached
# from `cron.schedule(...)` and from the GET /reports/monthly handler.

write_reports_package() {
  cat > "$REPORTS_DIR/package.json" <<'EOF'
{
  "name": "reports",
  "version": "0.1.0",
  "private": true,
  "dependencies": {
    "express": "^4.18.0",
    "node-cron": "^3.0.0"
  }
}
EOF
}

write_reports_main() {
  cat > "$REPORTS_DIR/src/index.ts" <<'EOF'
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

cron.schedule("0 0 1 * *", scheduledMonthlyReport);

app.listen(PORT);
EOF
}

write_reports() {
  mkdir -p "$REPORTS_DIR/src"
  cd "$REPORTS_DIR"
  git init -q -b main
  git config user.name "$GIT_AUTHOR_NAME"
  git config user.email "$GIT_AUTHOR_EMAIL"

  write_reports_package
  write_reports_main
  make_commit "reports: base" 0
  git tag base

  git checkout -q main
}

# --- platform repo -------------------------------------------------------
#
# Python monorepo. Two services:
#   services/shipping/  — calls inventory's GET /stock/{} via INVENTORY_URL
#   services/inventory/ — provides GET /stock/{}

write_platform_inventory() {
  cat > "$PLATFORM_DIR/services/inventory/main.py" <<'EOF'
"""Inventory service: stock-availability lookup."""
from fastapi import FastAPI

app = FastAPI()


@app.get("/stock/{sku}")
def get_stock(sku: str) -> dict[str, object]:
    return {"sku": sku, "available": True}
EOF
}

write_platform_shipping() {
  cat > "$PLATFORM_DIR/services/shipping/main.py" <<'EOF'
"""Shipping service: validates stock availability before promising delivery."""
import os

import httpx
from fastapi import FastAPI

INVENTORY_URL = os.environ["INVENTORY_URL"]

app = FastAPI()


def check_stock(sku: str) -> dict[str, object]:
    r = httpx.get(f"{INVENTORY_URL}/stock/{sku}")
    return r.json()


@app.get("/shipping/{sku}")
def get_shipping(sku: str) -> dict[str, object]:
    stock = check_stock(sku)
    return {"sku": sku, "available": stock.get("available", False)}
EOF
}

write_platform_requirements() {
  cat > "$PLATFORM_DIR/requirements.txt" <<'EOF'
fastapi==0.110.0
httpx==0.27.0
uvicorn==0.29.0
EOF
}

write_platform() {
  mkdir -p "$PLATFORM_DIR/services/inventory"
  mkdir -p "$PLATFORM_DIR/services/shipping"
  cd "$PLATFORM_DIR"
  git init -q -b main
  git config user.name "$GIT_AUTHOR_NAME"
  git config user.email "$GIT_AUTHOR_EMAIL"

  write_platform_requirements
  write_platform_inventory
  write_platform_shipping
  make_commit "platform: base" 0
  git tag base

  git checkout -q main
}

# --- repos.yaml ----------------------------------------------------------
#
# Generated from the four repos written above. All sources are
# `workspace_dir` (per the brief — no network). The `services` block
# is the only section that takes a position on per-service routing:
# orders and billing are reached by env-name, shipping/inventory are
# a monorepo split on path prefixes.

write_repos_yaml() {
  cat > "$ROOT/repos.yaml" <<EOF
data_dir: $ROOT/.lain-data
repos:
  - id: orders
    source:
      type: workspace_dir
      path: $ORDERS_DIR
  - id: billing
    source:
      type: workspace_dir
      path: $BILLING_DIR
  - id: reports
    source:
      type: workspace_dir
      path: $REPORTS_DIR
  - id: platform
    source:
      type: workspace_dir
      path: $PLATFORM_DIR
services:
  - name: orders
    repo: orders
    paths: []
    env: [ORDERS_URL]
  - name: billing
    repo: billing
    paths: []
    env: [BILLING_URL]
  - name: reports
    repo: reports
    paths: []
  - name: shipping
    repo: platform
    paths: [services/shipping/]
    env: [INVENTORY_URL]
  - name: inventory
    repo: platform
    paths: [services/inventory/]
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
  - GET /favicon.ico
bindings: []
EOF
}

# --- main ---------------------------------------------------------------

rm -rf "$ROOT"
mkdir -p "$ROOT"

write_orders
write_billing
write_reports
write_platform
write_repos_yaml

# Summary for the operator. Kept terse on purpose — the brief's
# verification step runs a checklist of files and tags by hand.
{
  echo "fixture: $ROOT"
  echo "orders tags: $(git -C "$ORDERS_DIR" tag --sort=refname | tr '\n' ' ')"
  echo "billing tags: $(git -C "$BILLING_DIR" tag --sort=refname | tr '\n' ' ')"
  echo "reports tags: $(git -C "$REPORTS_DIR" tag --sort=refname | tr '\n' ' ')"
  echo "platform tags: $(git -C "$PLATFORM_DIR" tag --sort=refname | tr '\n' ' ')"
} >&2
