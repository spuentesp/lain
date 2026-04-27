//! Protocol-first sensors
//!
//! Scans spec files to enrich existing graph nodes with cross-runtime
//! API surface information (gRPC, HTTP, GraphQL, WebSocket, etc.).

pub mod proto_sensor;
pub mod openapi_sensor;
pub mod graphql_sensor;
pub mod websocket_sensor;

pub use proto_sensor::ProtoService;
pub use openapi_sensor::OpenApiOperation;
pub use graphql_sensor::GraphQlOperation;
pub use websocket_sensor::WebSocketEndpoint;