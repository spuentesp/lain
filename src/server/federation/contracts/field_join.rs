//! Field join — `docs/CONTRACT_FEDERATION.md` §7.5.
//!
//! PR 9 fills the joiner step that PR 7 left as a documented no-op
//! (`// §7.5 field join lands with PR 9` in `joiner.rs:163`). For each
//! `FieldRef` f with `ReadsFrom` → call c, and each `Binds` (c →
//! provider route r) of endpoint e:
//!
//! 1. Take e's `Response` schema. If e has none, f stays unbound and
//!    e is listed in `schemaless_endpoints`.
//! 2. If f's chain equals a field's JSON path: `Binds` (f → that
//!    `Field`), with the read's provenance (`Static` when `exact`),
//!    capped by the endpoint bind.
//! 3. Else if f's chain is a suffix of exactly one field's path:
//!    `Binds`, `Heuristic { field_suffix }` 0.6, capped by the
//!    endpoint bind. If it is a suffix of several: one `Binds` each,
//!    `Heuristic { ambiguous_field }` 0.3.
//! 4. Else f is an **unknown-field read**, recorded on c's resolution
//!    and used by the consumer-side diff (§9.3).
//!
//! This module keeps the §7.5 logic out of `joiner.rs` so the
//! orchestration there stays focused on §7.2–§7.4 + §7.6 — the join
//! itself is a four-rule case analysis that benefits from living on
//! its own.

use std::collections::{BTreeMap, BTreeSet};

use crate::federation::contracts::index::{BoundField, EndpointId, FieldRefResolution};
use crate::federation::contracts::model::{
    ContractFact, ContractKey, Direction, FieldMeta, JsonPath, ServiceName,
};
use crate::federation::repo_id::GlobalId;
use crate::schema::{EdgeProvenance, GraphEdge, GraphNode};

use super::joiner::BindsEdge;

/// Schemas and fields indexed per endpoint, as collected from the
/// projected `Schema` / `Field` nodes and the `RequestSchema` /
/// `ResponseSchema` / `HasField` edges. The joiner hands this to
/// [`resolve_field_refs`] after step 4 has produced the `binds` set
/// (`Call` → `Endpoint`), so each `FieldRef` knows which endpoints
/// its `HttpClientCall` actually binds to.
#[derive(Debug, Default, Clone)]
pub(crate) struct EndpointSchemas {
    /// `EndpointId → Direction → Field list`. `Response` is the
    /// direction §7.5 reads against. `Request` is recorded for the
    /// eventual request-side diff; PR 9 only joins against response.
    pub by_endpoint: BTreeMap<EndpointId, BTreeMap<Direction, Vec<ResponseField>>>,
}

/// One flattened field of a response schema. The `(node_id, path)`
/// pair is what a `BoundField` records when the join succeeds.
#[derive(Debug, Clone)]
pub(crate) struct ResponseField {
    pub node_id: GlobalId,
    pub path: JsonPath,
    pub meta: FieldMeta,
}

/// Build [`EndpointSchemas`] from a flat list of projected nodes +
/// edges. The node list is the same one the joiner iterates
/// (`Schema` + `Field` + provider nodes). The edge list must include
/// every `RequestSchema` / `ResponseSchema` (`HttpRoute` → `Schema`)
/// and `HasField` (`Schema` → `Field`) edge the openapi sensor wrote.
///
/// The lookup keys endpoints by `(service, ContractKey)`, where the
/// `ContractKey` is the `(method, template)` pair of the response
/// schema's route — the joiner uses the same key for its endpoint
/// table, so a direct lookup against that table would also work, but
/// this module reads only the edges it needs (cost). A schema whose
/// route id does not resolve is dropped (the route was retracted
/// before its schema was — defensive).
pub(crate) fn collect_endpoint_schemas(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    assignments: &BTreeMap<String, ServiceName>,
) -> EndpointSchemas {
    // 1. Index every Schema + Field node by global id so we can
    //    resolve their contract payloads in O(1).
    let mut schema_by_id: BTreeMap<String, Direction> = BTreeMap::new();
    let mut field_node_by_id: BTreeMap<String, (JsonPath, FieldMeta)> = BTreeMap::new();
    for node in nodes {
        match node.contract.as_ref() {
            Some(ContractFact::Schema { direction }) => {
                schema_by_id.insert(node.id.clone(), *direction);
            }
            Some(ContractFact::Field(meta)) => {
                // The Field name is the JSON path (§6.4); recover
                // the structured form by parsing the name back. This
                // is the round-trip already pinned in the model
                // tests; if the openapi sensor ever emits a path
                // that doesn't parse, fall back to the empty path.
                let parsed = node
                    .name
                    .parse::<JsonPath>()
                    .unwrap_or_else(|_| JsonPath(Vec::new()));
                field_node_by_id.insert(node.id.clone(), (parsed, meta.clone()));
            }
            _ => {}
        }
    }

    // 2. Schema id → list of fields. BTreeMap preserves insertion
    //    order so deterministic output follows the (path-sorted) order
    //    the openapi sensor emits.
    let mut fields_by_schema: BTreeMap<String, Vec<ResponseField>> = BTreeMap::new();
    for edge in edges {
        if edge.edge_type != crate::schema::EdgeType::HasField {
            continue;
        }
        let Some((path, meta)) = field_node_by_id.get(&edge.target_id) else {
            continue;
        };
        let Ok(gid) = GlobalId::parse(&edge.target_id) else {
            continue;
        };
        fields_by_schema
            .entry(edge.source_id.clone())
            .or_default()
            .push(ResponseField {
                node_id: gid,
                path: path.clone(),
                meta: meta.clone(),
            });
    }

    // 3. Route id → list of `(direction, schema_id)`. The route id
    //    is the `HttpRoute` node's global id; we resolve the service
    //    from the assignments map.
    let mut schemas_by_route: BTreeMap<String, Vec<(Direction, String)>> = BTreeMap::new();
    for edge in edges {
        let dir = match edge.edge_type {
            crate::schema::EdgeType::RequestSchema => Some(Direction::Request),
            crate::schema::EdgeType::ResponseSchema => Some(Direction::Response),
            _ => None,
        };
        let Some(dir) = dir else { continue };
        schemas_by_route
            .entry(edge.source_id.clone())
            .or_default()
            .push((dir, edge.target_id.clone()));
    }

    // 4. For every route that has at least one schema, resolve its
    //    service and `(method, template)` so we can index by
    //    `EndpointId`. Routes without a service assignment are
    //    dropped — they live in the per-repo graph but the
    //    federation can't route them.
    let mut by_endpoint: BTreeMap<EndpointId, BTreeMap<Direction, Vec<ResponseField>>> =
        BTreeMap::new();
    for (route_id, schemas) in schemas_by_route {
        let Some(svc) = assignments.get(&route_id) else {
            continue;
        };
        // The route's `ContractKey` is `(method, template)` — recover
        // both from the route's `ProviderFact`.
        let Some(route_node) = nodes.iter().find(|n| n.id == route_id) else {
            continue;
        };
        let Some(ContractFact::Provider(p)) = route_node.contract.as_ref() else {
            continue;
        };
        let key = ContractKey::Http {
            method: crate::federation::contracts::model::MethodSpec::Known(p.method),
            template: p.template.clone(),
        };
        let endpoint_id: EndpointId = (svc.clone(), key);
        let bucket = by_endpoint.entry(endpoint_id).or_default();
        for (dir, schema_id) in schemas {
            if let Some(fields) = fields_by_schema.get(&schema_id) {
                bucket
                    .entry(dir)
                    .or_default()
                    .extend(fields.iter().cloned());
            }
        }
    }

    EndpointSchemas { by_endpoint }
}

/// Resolve every `FieldRef` against the bound endpoints of its
/// `HttpClientCall`. The output is the `field_refs` map on
/// [`crate::federation::contracts::index::ContractIndex`].
///
/// `field_ref_nodes` is every `FieldRef` node the joiner sees (the
/// sensor is owner-only; only its emitted nodes reach the joiner).
/// `reads_from_edges` is the `ReadsFrom` set: `(FieldRef id → call
/// id)`. `binds` is the joiner's own step 4 / step 6 output: for each
/// `HttpClientCall`, the list of endpoints it is bound to (one per
/// rule 3 / rule 6 hit; multiple for rule 6 ambiguity). `schemas`
/// comes from [`collect_endpoint_schemas`].
///
/// The function is deterministic: every collection is a `BTreeMap`,
/// and output iterates in `(field_ref_id, endpoint_id)` order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_field_refs(
    field_ref_nodes: &[GraphNode],
    reads_from_edges: &[GraphEdge],
    binds: &[BindsEdge],
    schemas: &EndpointSchemas,
) -> (
    BTreeMap<GlobalId, FieldRefResolution>,
    BTreeSet<EndpointId>,
    BTreeSet<GlobalId>,
) {
    // Build call → [endpoint] from the bind edges. `BindsEdge`
    // already carries `target_endpoint` (the EndpointId the call
    // joined against); we just key it by `consumer` (the call's
    // GlobalId).
    let mut call_to_endpoints: BTreeMap<String, BTreeSet<EndpointId>> = BTreeMap::new();
    for b in binds {
        call_to_endpoints
            .entry(b.consumer.as_str().to_string())
            .or_default()
            .insert(b.target_endpoint.clone());
    }

    // FieldRef id → call id (a FieldRef can read from only one
    // HttpClientCall per `ReadsFrom` edge — multiple `ReadsFrom`
    // edges per FieldRef would be a sensor bug).
    let mut reads_from: BTreeMap<String, String> = BTreeMap::new();
    for edge in reads_from_edges {
        if edge.edge_type != crate::schema::EdgeType::ReadsFrom {
            continue;
        }
        reads_from.insert(edge.source_id.clone(), edge.target_id.clone());
    }

    let mut out: BTreeMap<GlobalId, FieldRefResolution> = BTreeMap::new();
    let mut schemaless_endpoints: BTreeSet<EndpointId> = BTreeSet::new();
    let mut unknown_field_refs: BTreeSet<GlobalId> = BTreeSet::new();

    for node in field_ref_nodes {
        let Ok(fid) = GlobalId::parse(&node.id) else {
            continue;
        };
        let Some(ContractFact::FieldRead(read)) = node.contract.as_ref() else {
            continue;
        };
        let Some(call_id) = reads_from.get(&node.id) else {
            // No `ReadsFrom` edge — the FieldRef is orphaned. The
            // sensor guarantees one; if it didn't, the read has no
            // call to join against and is recorded as unknown.
            let resolution = FieldRefResolution {
                field_ref_id: fid.clone(),
                service: ServiceName("unknown".into()),
                bound_fields: Vec::new(),
                unknown: true,
            };
            out.insert(fid.clone(), resolution);
            unknown_field_refs.insert(fid);
            continue;
        };
        let Some(endpoints) = call_to_endpoints.get(call_id) else {
            // Call is not bound to any endpoint (unresolved / external
            // / unnormalized). Per §7.5 there's no endpoint to join
            // against; record the FieldRef as unknown and let the
            // consumer-side diff surface it (§9.3).
            let resolution = FieldRefResolution {
                field_ref_id: fid.clone(),
                service: ServiceName("unknown".into()),
                bound_fields: Vec::new(),
                unknown: true,
            };
            out.insert(fid.clone(), resolution);
            unknown_field_refs.insert(fid);
            continue;
        };
        // Per §7.5 step 1: an endpoint with no `Response` schema
        // becomes `schemaless_endpoints`; the FieldRef stays unbound
        // but is not marked unknown (the schema is just missing).
        // §7.5 step 4 (unknown-field read) only fires when an
        // endpoint has a schema that doesn't cover the read chain.
        let mut bound_any = false;
        let mut unknown_any = false;
        let mut bound_fields: Vec<BoundField> = Vec::new();
        for endpoint in endpoints {
            let response_fields = match schemas.by_endpoint.get(endpoint) {
                Some(dirs) => dirs.get(&Direction::Response),
                None => None,
            };
            let Some(response_fields) = response_fields else {
                schemaless_endpoints.insert(endpoint.clone());
                continue;
            };
            if response_fields.is_empty() {
                schemaless_endpoints.insert(endpoint.clone());
                continue;
            }
            // §7.5 step 2 — exact match on JSON path.
            let exact: Vec<&ResponseField> = response_fields
                .iter()
                .filter(|f| f.path == read.chain)
                .collect();
            if let Some(field) = exact.first() {
                let provenance = read_provenance(read.exact);
                let confidence = match &provenance {
                    EdgeProvenance::Static { .. } => 1.0,
                    EdgeProvenance::Heuristic { confidence, .. } => *confidence,
                    _ => 1.0,
                };
                bound_fields.push(BoundField {
                    field: field.node_id.clone(),
                    field_path: field.path.clone(),
                    endpoint: endpoint.clone(),
                    confidence,
                });
                bound_any = true;
                continue;
            }
            // §7.5 step 3 — suffix matches.
            let suffix_matches: Vec<&ResponseField> = response_fields
                .iter()
                .filter(|f| is_suffix(&f.path, &read.chain))
                .collect();
            if suffix_matches.len() == 1 {
                let field = suffix_matches[0];
                bound_fields.push(BoundField {
                    field: field.node_id.clone(),
                    field_path: field.path.clone(),
                    endpoint: endpoint.clone(),
                    confidence: 0.6,
                });
                bound_any = true;
                continue;
            }
            if suffix_matches.len() > 1 {
                for field in suffix_matches {
                    bound_fields.push(BoundField {
                        field: field.node_id.clone(),
                        field_path: field.path.clone(),
                        endpoint: endpoint.clone(),
                        confidence: 0.3,
                    });
                }
                bound_any = true;
                continue;
            }
            // §7.5 step 4 — no exact match, no suffix match on this
            // endpoint. The endpoint has a schema, so it isn't
            // schemaless; we mark `unknown_any` so the outer logic
            // surfaces the read on c's resolution (used by the
            // consumer-side diff in §9.3).
            unknown_any = true;
        }
        let resolution = if bound_any {
            // Per §7.5 the read's provenance caps the join confidence
            // (`Static` only on exact; otherwise `Heuristic`). The
            // confidence we attach to `BoundField` already encodes
            // that (1.0 for `Static`, 0.6 / 0.3 for suffix / ambiguous
            // — the suffix / ambiguous values are themselves the
            // "Heuristic" rule's confidence).
            FieldRefResolution {
                field_ref_id: fid.clone(),
                service: endpoint_service(endpoints),
                bound_fields,
                unknown: false,
            }
        } else {
            // No bound field — either every endpoint was schemaless
            // (the resolution isn't unknown, just unbound) or at
            // least one endpoint had a schema that didn't cover the
            // read chain (the resolution is unknown). Track both
            // cases with `unknown_any` so a FieldRef that *only*
            // touched schemaless endpoints stays `unknown = false`.
            FieldRefResolution {
                field_ref_id: fid.clone(),
                service: endpoint_service(endpoints),
                bound_fields: Vec::new(),
                unknown: unknown_any,
            }
        };
        if resolution.unknown {
            unknown_field_refs.insert(fid.clone());
        }
        out.insert(fid, resolution);
    }

    (out, schemaless_endpoints, unknown_field_refs)
}

/// A read is `Static { TreeSitter }` when the chain is exact and the
/// sensor recorded `exact = true` (literal key access on a bound
/// identifier). A non-exact read is `Heuristic { field_suffix }` —
/// the joiner's default — because a destructured chain or a non-
/// literal key on the read side already weakened the chain.
pub(crate) fn read_provenance(exact: bool) -> EdgeProvenance {
    if exact {
        EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        }
    } else {
        EdgeProvenance::Heuristic {
            detector: "field_suffix".into(),
            confidence: 0.6,
        }
    }
}

/// `read_chain` is a suffix of `field_path` when the trailing
/// segments of `field_path` equal `read_chain`. Used for the §7.5
/// step-3 suffix match. The empty read chain (`[]`) is a suffix of
/// every path — that matches "read the whole response as one blob",
/// which the brief treats as a step-4 unknown-field read (no usable
/// signal in the chain), so this function returns `false` for the
/// empty chain and the joiner falls through to step 4.
pub(crate) fn is_suffix(field_path: &JsonPath, read_chain: &JsonPath) -> bool {
    let read = &read_chain.0;
    if read.is_empty() {
        return false;
    }
    let fp = &field_path.0;
    if fp.len() < read.len() {
        return false;
    }
    fp[fp.len() - read.len()..] == read[..]
}

fn endpoint_service(endpoints: &BTreeSet<EndpointId>) -> ServiceName {
    endpoints
        .iter()
        .next()
        .map(|(svc, _)| svc.clone())
        .unwrap_or_else(|| ServiceName("unknown".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::{HttpMethod, MethodSpec, TypeDesc};

    fn http_key(method: HttpMethod, template: &str) -> ContractKey {
        ContractKey::Http {
            method: MethodSpec::Known(method),
            template: template.to_string(),
        }
    }

    fn field(node_id: &str, path: &str) -> ResponseField {
        ResponseField {
            node_id: GlobalId::parse(node_id).unwrap(),
            path: path.parse().unwrap(),
            meta: FieldMeta {
                ty: TypeDesc::String,
                required: true,
                nullable: false,
                enum_values: None,
            },
        }
    }

    #[test]
    fn read_provenance_is_static_when_exact() {
        let p = read_provenance(true);
        assert!(matches!(p, EdgeProvenance::Static { .. }));
    }

    #[test]
    fn read_provenance_is_heuristic_when_not_exact() {
        let p = read_provenance(false);
        match p {
            EdgeProvenance::Heuristic {
                detector,
                confidence,
            } => {
                assert_eq!(detector, "field_suffix");
                assert!((confidence - 0.6).abs() < f32::EPSILON);
            }
            other => panic!("expected Heuristic, got {other:?}"),
        }
    }

    #[test]
    fn is_suffix_matches_a_trailing_segment() {
        let fp: JsonPath = "customer.id".parse().unwrap();
        let rc: JsonPath = "id".parse().unwrap();
        assert!(is_suffix(&fp, &rc));
        let rc2: JsonPath = "customer.id".parse().unwrap();
        assert!(is_suffix(&fp, &rc2));
        let rc3: JsonPath = "address.id".parse().unwrap();
        assert!(!is_suffix(&fp, &rc3));
    }

    #[test]
    fn is_suffix_does_not_match_an_empty_chain() {
        let fp: JsonPath = "customer.id".parse().unwrap();
        let empty = JsonPath(Vec::new());
        assert!(!is_suffix(&fp, &empty));
    }

    #[test]
    fn is_suffix_handles_array_segments() {
        let fp: JsonPath = "items[].sku".parse().unwrap();
        let rc: JsonPath = "items[].sku".parse().unwrap();
        assert!(is_suffix(&fp, &rc));
        let rc_short: JsonPath = "[].sku".parse().unwrap();
        assert!(is_suffix(&fp, &rc_short));
    }

    fn schemas_with(endpoint: EndpointId, fields: Vec<ResponseField>) -> EndpointSchemas {
        let mut by_endpoint: BTreeMap<EndpointId, BTreeMap<Direction, Vec<ResponseField>>> =
            BTreeMap::new();
        let mut dir_map: BTreeMap<Direction, Vec<ResponseField>> = BTreeMap::new();
        dir_map.insert(Direction::Response, fields);
        by_endpoint.insert(endpoint, dir_map);
        EndpointSchemas { by_endpoint }
    }

    #[test]
    fn exact_match_yields_one_bound_field_with_static_confidence() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(
            endpoint.clone(),
            vec![field("orders:Field:openapi.yaml:id:10", "customer.id")],
        );
        let fr_node = node_field_ref("customer.id", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        let (out, schemaless, unknown) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert!(schemaless.is_empty());
        assert!(unknown.is_empty());
        assert_eq!(out.len(), 1);
        let r = out.values().next().unwrap();
        assert_eq!(r.bound_fields.len(), 1);
        assert!((r.bound_fields[0].confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn unique_suffix_match_uses_field_suffix_confidence() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(
            endpoint.clone(),
            vec![field(
                "orders:Field:openapi.yaml:customer.id:5",
                "customer.id",
            )],
        );
        let fr_node = node_field_ref("id", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        let (out, schemaless, _unknown) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert!(schemaless.is_empty());
        let r = out.values().next().unwrap();
        assert_eq!(r.bound_fields.len(), 1);
        assert!((r.bound_fields[0].confidence - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn ambiguous_suffix_match_binds_every_candidate_at_0_3() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(
            endpoint.clone(),
            vec![
                field("orders:Field:openapi.yaml:customer.id:5", "customer.id"),
                field("orders:Field:openapi.yaml:address.id:7", "address.id"),
            ],
        );
        let fr_node = node_field_ref("id", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        let (out, schemaless, _unknown) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert!(schemaless.is_empty());
        let r = out.values().next().unwrap();
        assert_eq!(r.bound_fields.len(), 2, "two ambiguous suffix matches");
        for b in &r.bound_fields {
            assert!((b.confidence - 0.3).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn unknown_field_read_is_marked_when_no_path_matches() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(
            endpoint.clone(),
            vec![field(
                "orders:Field:openapi.yaml:customer.id:5",
                "customer.id",
            )],
        );
        let fr_node = node_field_ref("discount", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        let (out, schemaless, unknown) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert!(schemaless.is_empty());
        let r = out.values().next().unwrap();
        assert!(r.unknown);
        assert_eq!(unknown.len(), 1);
    }

    #[test]
    fn endpoint_without_a_response_schema_is_schemaless() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = EndpointSchemas::default();
        let fr_node = node_field_ref("id", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        let (out, schemaless, _unknown) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert_eq!(schemaless.len(), 1);
        let r = out.values().next().unwrap();
        assert!(!r.unknown, "schemaless is not unknown — just unbounded");
        assert!(r.bound_fields.is_empty());
    }

    #[test]
    fn field_ref_with_no_reads_from_edge_is_unknown() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(endpoint.clone(), vec![]);
        let fr_node = node_field_ref("id", true);
        let (out, _schemaless, unknown) = resolve_field_refs(&[fr_node], &[], &[], &schemas);
        assert_eq!(unknown.len(), 1);
        assert!(out.values().next().unwrap().unknown);
    }

    #[test]
    fn call_with_no_binds_endpoint_marks_field_ref_unknown() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(endpoint.clone(), vec![]);
        let fr_node = node_field_ref("id", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let (out, _schemaless, unknown) =
            resolve_field_refs(&[fr_node], &reads_from, &[], &schemas);
        assert_eq!(unknown.len(), 1);
        assert!(out.values().next().unwrap().unknown);
    }

    #[test]
    fn output_is_sorted_for_determinism() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(endpoint.clone(), vec![]);
        // Two FieldRefs in arbitrary order — the output map should be
        // ordered by GlobalId string (BTreeMap).
        let mut n1 = node_field_ref("a", true);
        n1.id = "billing:FieldRef:a.py:a:1".into();
        let mut n2 = node_field_ref("b", true);
        n2.id = "billing:FieldRef:a.py:b:2".into();
        let out = resolve_field_refs(&[n2.clone(), n1.clone()], &[], &[], &schemas);
        let keys: Vec<String> = out.0.keys().map(|k| k.as_str().to_string()).collect();
        assert!(keys[0] < keys[1], "BTreeMap keys are sorted: {keys:?}");
    }

    // ─── Helpers ──────────────────────────────────────────────────

    fn node_field_ref(chain: &str, exact: bool) -> GraphNode {
        use crate::federation::contracts::model::FieldReadFact;
        let mut node = GraphNode::new_in(
            crate::schema::NodeType::FieldRef,
            chain.to_string(),
            "billing/x.py".into(),
            &crate::schema::RepoNamespace::for_test(),
        );
        node.id = format!("billing:FieldRef:billing.py:{chain}:1");
        node.repo_id = Some("billing".into());
        node.line_start = Some(1);
        node.contract = Some(ContractFact::FieldRead(FieldReadFact {
            chain: chain.parse().unwrap(),
            exact,
        }));
        node
    }

    fn edge(edge_type: crate::schema::EdgeType, source: &str, target: &str) -> GraphEdge {
        let mut e = GraphEdge::new(edge_type, source.into(), target.into());
        e.provenance = Some(EdgeProvenance::Static {
            source: crate::schema::StaticSource::TreeSitter,
        });
        e
    }

    fn bind_edge(consumer: &str, endpoint: &EndpointId) -> BindsEdge {
        BindsEdge {
            consumer: GlobalId::parse(consumer).unwrap(),
            provider: GlobalId::parse("orders:HttpRoute:o.py:list:1").unwrap(),
            consumer_service: ServiceName("billing".into()),
            provider_service: endpoint.0.clone(),
            target_endpoint: endpoint.clone(),
            provenance: EdgeProvenance::Heuristic {
                detector: "static".into(),
                confidence: 1.0,
            },
            confidence: 1.0,
            route_match: crate::schema::RouteMatch::Exact,
            stripped_prefix: None,
        }
    }
}
