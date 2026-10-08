//! Field join — `docs/superpowers/specs/2026-10-02-contract-coverage-and-protocols-design.md` §7.5.
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
    ContractFact, Direction, FieldMeta, JsonPath, ServiceName,
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
    /// `EndpointId → Direction → Schema node id` — the openapi
    /// `Schema` node the direction's fields hang off (`HasField`).
    /// `EndpointSchema::node_id` carries this so tools can walk
    /// `Schema → Field` (e.g. `trace_impact`'s field arm) instead
    /// of mistaking the provider route for the schema.
    pub schema_node: BTreeMap<EndpointId, BTreeMap<Direction, GlobalId>>,
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
    config: &crate::federation::contracts::config::ContractFederationConfig,
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
    //    is the route or topic node's global id; we resolve the service
    //    from the assignments map.
    let mut schemas_by_route: BTreeMap<String, Vec<(Direction, String)>> = BTreeMap::new();
    for edge in edges {
        let dir = match edge.edge_type {
            crate::schema::EdgeType::RequestSchema => Some(Direction::Request),
            crate::schema::EdgeType::ResponseSchema => Some(Direction::Response),
            crate::schema::EdgeType::PayloadSchema => Some(Direction::Payload),
            _ => None,
        };
        let Some(dir) = dir else { continue };
        schemas_by_route
            .entry(edge.source_id.clone())
            .or_default()
            .push((dir, edge.target_id.clone()));
    }

    // 4. For every route that has at least one schema, resolve its
    //    service and ContractKey so we can index by `EndpointId`.
    let mut by_endpoint: BTreeMap<EndpointId, BTreeMap<Direction, Vec<ResponseField>>> =
        BTreeMap::new();
    let mut schema_node: BTreeMap<EndpointId, BTreeMap<Direction, GlobalId>> = BTreeMap::new();
    for (route_id, schemas) in schemas_by_route {
        let Some(svc) = assignments.get(&route_id) else {
            continue;
        };
        let Some(route_node) = nodes.iter().find(|n| n.id == route_id) else {
            continue;
        };
        // Task 4: derive the key via the same function the endpoint
        // table uses, so a `base_path` / `route_prefixes` on the
        // service produces the same key in both places.
        let Some((endpoint_id, _template)) =
            super::joiner::endpoints::contract_key_for_provider(route_node, svc, config)
        else {
            continue;
        };
        let bucket = by_endpoint.entry(endpoint_id.clone()).or_default();
        let mut node_map: BTreeMap<Direction, GlobalId> = BTreeMap::new();
        for (dir, schema_id) in schemas {
            if let Ok(gid) = GlobalId::parse(&schema_id) {
                node_map.entry(dir).or_insert(gid);
            }
            if let Some(fields) = fields_by_schema.get(&schema_id) {
                bucket
                    .entry(dir)
                    .or_default()
                    .extend(fields.iter().cloned());
            }
        }
        if !node_map.is_empty() {
            schema_node.insert(endpoint_id, node_map);
        }
    }

    // Step 4b: Schemas explicitly declared in repos.yaml via `schemas` (Gap 20).
    //
    // Task 4: the pre-fix code built the endpoint id from
    // `decl.repo` (treated as a service name) and `"kafka"`
    // (hardcoded broker), so any service whose configured name
    // differed from its repo id (e.g. `payments-api` over repo
    // `payments`) or whose topic sat on a non-kafka broker never
    // reached the endpoint the joiner built. Fix: look up the
    // topic provider that actually emits `decl.topic` and reuse
    // `contract_key_for_provider` for the broker + full template,
    // so the key byte-matches the one `build_endpoints` produced.
    for decl in &config.schemas {
        let Some(schema_node_rec) = pick_schema_node(nodes, &decl.repo, &decl.file, None) else {
            continue;
        };
        let Some(fields) = fields_by_schema.get(&schema_node_rec.id) else {
            continue;
        };
        let Ok(schema_gid) = GlobalId::parse(&schema_node_rec.id) else {
            continue;
        };
        let Some(svc) = assignments.get(&schema_node_rec.id).cloned() else {
            continue;
        };

        // Verify the matched schema node belongs to a service
        // whose configured repo is `decl.repo`. `validate()` is
        // the strict gate that catches the misconfig, but the
        // joiner can be invoked without it (e.g. from tests
        // that hand-build the inputs). Without this check the
        // schema would silently attach to the wrong endpoint
        // whenever the operator typoed the repo.
        let svc_repo_matches = config
            .services
            .iter()
            .find(|s| s.name == svc.0)
            .map(|s| s.repo == decl.repo)
            .unwrap_or(false);
        if !svc_repo_matches {
            continue;
        }

        // Find the topic provider that emits `decl.topic` from
        // the same service. `validate()` already guarantees a
        // service exists for `decl.repo`, but a deployment may
        // publish the topic from a sibling service inside the
        // same repo — fall back to any matching topic provider
        // so the schema reaches the right endpoint.
        let topic_node = nodes
            .iter()
            .find(|n| {
                n.node_type == crate::schema::NodeType::Topic
                    && matches!(n.contract.as_ref(), Some(ContractFact::Provider(p)) if p.template == decl.topic)
                    && assignments.get(&n.id) == Some(&svc)
            })
            .or_else(|| {
                nodes.iter().find(|n| {
                    n.node_type == crate::schema::NodeType::Topic
                        && matches!(n.contract.as_ref(), Some(ContractFact::Provider(p)) if p.template == decl.topic)
                })
            });
        let Some(topic_node) = topic_node else {
            continue;
        };
        let Some((endpoint_id, _template)) =
            super::joiner::endpoints::contract_key_for_provider(topic_node, &svc, config)
        else {
            continue;
        };
        let bucket = by_endpoint.entry(endpoint_id.clone()).or_default();
        bucket
            .entry(Direction::Payload)
            .or_default()
            .extend(fields.iter().cloned());
        schema_node
            .entry(endpoint_id)
            .or_default()
            .insert(Direction::Payload, schema_gid);
    }

    EndpointSchemas {
        by_endpoint,
        schema_node,
    }
}

/// Step 4b's Schema-node lookup: among the `Schema` nodes whose path
/// is `file` (or ends in `/file`), pick the one a `repos.yaml`
/// `schemas:` entry refers to — deterministically (I4). Several
/// types can share one file (a multi-type SDL), and `nodes` order
/// comes from a DashMap iteration, so "first match" would make the
/// output depend on insertion order. The pick is a total order on
/// the node itself, never on iteration order: an exact `name` match
/// first (when the caller knows the declared type), then a node from
/// the declared `repo`, then the lowest `line_start` (`None` counts
/// as `u32::MAX` — an unpositioned node cannot outrank a positioned
/// one), then the lexicographically smallest `GlobalId`.
fn pick_schema_node<'a>(
    nodes: &'a [GraphNode],
    repo: &str,
    file: &str,
    name: Option<&str>,
) -> Option<&'a GraphNode> {
    nodes
        .iter()
        .filter(|n| {
            n.node_type == crate::schema::NodeType::Schema
                && (n.path == file || n.path.ends_with(&format!("/{file}")))
        })
        .min_by(|a, b| {
            let a_name = name.is_some_and(|w| a.name == w);
            let b_name = name.is_some_and(|w| b.name == w);
            let a_repo = a.repo_id.as_deref() == Some(repo);
            let b_repo = b.repo_id.as_deref() == Some(repo);
            b_name
                .cmp(&a_name)
                .then_with(|| b_repo.cmp(&a_repo))
                .then_with(|| {
                    a.line_start
                        .unwrap_or(u32::MAX)
                        .cmp(&b.line_start.unwrap_or(u32::MAX))
                })
                .then_with(|| a.id.cmp(&b.id))
        })
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
///
/// Returns `(resolutions, schemaless_endpoints, unknown_field_refs,
/// field_binds)` — the fourth element is the §7.5 step-2/3
/// `Binds(FieldRef → Field)` edges the joiner merges into its output
/// so they reach the graph (§4.2 edge table; §9.5 traces
/// `Field ← Binds ← FieldRef`).
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
    Vec<BindsEdge>,
) {
    // Build call → [endpoint → the endpoint-side bind a field join
    // is capped by] from the joiner's step 4 / step 6 output.
    // `BindsEdge` already carries `target_endpoint` (the EndpointId
    // the call joined against); we key it by `consumer` (the call's
    // GlobalId). The same (call, endpoint) can appear twice — a
    // rule-3 hit plus a §7.6 Confirmed override — so the merge
    // picks the bind the joiner's step-7 dedup would keep: lowest
    // confidence, ties broken by the provenance's `Debug` ordering
    // (`Confirmed…` < `Heuristic…`), keeping the cap independent of
    // input order (§7.8 purity).
    let mut call_to_endpoints: BTreeMap<String, BTreeMap<EndpointId, EndpointBindInfo>> =
        BTreeMap::new();
    for b in binds {
        let info = EndpointBindInfo {
            confidence: b.confidence,
            provenance: b.provenance.clone(),
            route_match: b.route_match,
            stripped_prefix: b.stripped_prefix.clone(),
            consumer_service: b.consumer_service.clone(),
            provider_service: b.provider_service.clone(),
        };
        let entry = call_to_endpoints
            .entry(b.consumer.as_str().to_string())
            .or_default()
            .entry(b.target_endpoint.clone())
            .or_insert_with(|| info.clone());
        // Order-independent merge: keep the weaker / deterministic
        // winner (see the doc above).
        let entry_key = (entry.confidence, format!("{:?}", entry.provenance));
        let info_key = (info.confidence, format!("{:?}", info.provenance));
        if info_key < entry_key {
            *entry = info;
        }
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
    let mut field_binds: Vec<BindsEdge> = Vec::new();

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
                call: String::new(),
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
                call: call_id.clone(),
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
        for (endpoint, ep_bind) in endpoints {
            let response_fields = match schemas.by_endpoint.get(endpoint) {
                Some(dirs) => dirs
                    .get(&Direction::Response)
                    .or_else(|| dirs.get(&Direction::Payload)),
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
            // §7.5 step 2 — exact match on JSON path. The join is
            // `Binds(f → that Field)`, capped by the endpoint bind
            // (§4.6: min of both, `Heuristic` if either).
            let exact: Vec<&ResponseField> = response_fields
                .iter()
                .filter(|f| f.path == read.chain)
                .collect();
            if let Some(field) = exact.first() {
                let (provenance, confidence) =
                    cap_field_bind(&read_provenance(read.exact), ep_bind);
                bound_fields.push(BoundField {
                    field: field.node_id.clone(),
                    field_path: field.path.clone(),
                    endpoint: endpoint.clone(),
                    confidence,
                });
                field_binds.push(make_field_bind(
                    &fid, field, endpoint, ep_bind, provenance, confidence,
                ));
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
                let read_prov = EdgeProvenance::Heuristic {
                    detector: "field_suffix".into(),
                    confidence: 0.6,
                };
                let (provenance, confidence) = cap_field_bind(&read_prov, ep_bind);
                bound_fields.push(BoundField {
                    field: field.node_id.clone(),
                    field_path: field.path.clone(),
                    endpoint: endpoint.clone(),
                    confidence,
                });
                field_binds.push(make_field_bind(
                    &fid, field, endpoint, ep_bind, provenance, confidence,
                ));
                bound_any = true;
                continue;
            }
            if suffix_matches.len() > 1 {
                for field in suffix_matches {
                    let read_prov = EdgeProvenance::Heuristic {
                        detector: "ambiguous_field".into(),
                        confidence: 0.3,
                    };
                    let (provenance, confidence) = cap_field_bind(&read_prov, ep_bind);
                    bound_fields.push(BoundField {
                        field: field.node_id.clone(),
                        field_path: field.path.clone(),
                        endpoint: endpoint.clone(),
                        confidence,
                    });
                    field_binds.push(make_field_bind(
                        &fid, field, endpoint, ep_bind, provenance, confidence,
                    ));
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
            // Per §7.5 + §4.6 the field-bind confidence is the
            // read's match confidence (1.0 exact / 0.6 suffix /
            // 0.3 ambiguous) capped by the endpoint bind — the
            // `cap_field_bind` result already written into both
            // `BoundField.confidence` and the emitted `Binds` edge.
            FieldRefResolution {
                field_ref_id: fid.clone(),
                service: endpoint_service(endpoints),
                bound_fields,
                unknown: false,
                call: call_id.clone(),
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
                call: call_id.clone(),
            }
        };
        if resolution.unknown {
            unknown_field_refs.insert(fid.clone());
        }
        out.insert(fid, resolution);
    }

    // §7.8 purity: the joiner's `nodes` slice order comes from a
    // DashMap iteration, so sort the emitted edges here — the final
    // step-7 sort is stable and keeps this order for identical
    // `(consumer, provider)` keys (a FieldRef matched via two
    // endpoints to the same Field node collapses deterministically).
    field_binds.sort_by(|a, b| {
        a.consumer
            .as_str()
            .cmp(b.consumer.as_str())
            .then_with(|| a.provider.as_str().cmp(b.provider.as_str()))
            .then_with(|| a.target_endpoint.cmp(&b.target_endpoint))
    });

    (out, schemaless_endpoints, unknown_field_refs, field_binds)
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

/// The endpoint-side bind a field join is derived from and capped
/// by (§7.5 step 2/3 "capped by the endpoint bind"). Carries the
/// pieces the emitted `Binds(FieldRef → Field)` edge inherits.
#[derive(Clone)]
struct EndpointBindInfo {
    confidence: f32,
    provenance: EdgeProvenance,
    route_match: crate::schema::RouteMatch,
    stripped_prefix: Option<String>,
    consumer_service: ServiceName,
    provider_service: ServiceName,
}

/// Numeric confidence of a provenance, for the §4.6 minimum.
fn prov_confidence(p: &EdgeProvenance) -> f32 {
    match p {
        EdgeProvenance::Static { .. } | EdgeProvenance::Confirmed { .. } => 1.0,
        EdgeProvenance::Heuristic { confidence, .. } => *confidence,
        // Runtime observations carry no confidence scalar; treat as
        // certain (they only appear on non-contract edges in practice).
        EdgeProvenance::Runtime { .. } => 1.0,
    }
}

/// §4.6: "Where a rule below says 'capped by' another edge, the new
/// edge's confidence is the minimum of both, and it is `Heuristic`
/// if either is." Returns the capped provenance (preserving the
/// detector detail of the weaker source) and the capped confidence.
fn cap_field_bind(read: &EdgeProvenance, endpoint: &EndpointBindInfo) -> (EdgeProvenance, f32) {
    let r_conf = prov_confidence(read);
    let cap = r_conf.min(endpoint.confidence);
    let r_heuristic = matches!(read, EdgeProvenance::Heuristic { .. });
    let e_heuristic = matches!(endpoint.provenance, EdgeProvenance::Heuristic { .. });
    let provenance = if r_heuristic && e_heuristic {
        // Both sides are weakened: keep the weaker source's detail
        // (tie → the read side, deterministic either way).
        if r_conf <= endpoint.confidence {
            read.clone()
        } else {
            endpoint.provenance.clone()
        }
    } else if e_heuristic {
        endpoint.provenance.clone()
    } else {
        read.clone()
    };
    (provenance, cap)
}

/// Build one §7.5 `Binds(FieldRef → Field)` edge. The edge inherits
/// the endpoint bind's services, route detail, and the *capped*
/// provenance/confidence from [`cap_field_bind`].
fn make_field_bind(
    field_ref: &GlobalId,
    field: &ResponseField,
    endpoint: &EndpointId,
    ep_bind: &EndpointBindInfo,
    provenance: EdgeProvenance,
    confidence: f32,
) -> BindsEdge {
    BindsEdge {
        consumer: field_ref.clone(),
        provider: field.node_id.clone(),
        consumer_service: ep_bind.consumer_service.clone(),
        provider_service: ep_bind.provider_service.clone(),
        target_endpoint: endpoint.clone(),
        provenance,
        confidence,
        route_match: ep_bind.route_match,
        stripped_prefix: ep_bind.stripped_prefix.clone(),
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

fn endpoint_service(endpoints: &BTreeMap<EndpointId, EndpointBindInfo>) -> ServiceName {
    endpoints
        .keys()
        .next()
        .map(|(svc, _)| svc.clone())
        .unwrap_or_else(|| ServiceName("unknown".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::{
        ContractKey, HttpMethod, MethodSpec, ProviderFact, ProviderOrigin, TypeDesc,
    };

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
        EndpointSchemas {
            by_endpoint,
            schema_node: BTreeMap::new(),
        }
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
        let (out, schemaless, unknown, field_binds) = resolve_field_refs(
            std::slice::from_ref(&fr_node),
            &reads_from,
            &binds,
            &schemas,
        );
        assert!(schemaless.is_empty());
        assert!(unknown.is_empty());
        assert_eq!(out.len(), 1);
        let r = out.values().next().unwrap();
        assert_eq!(r.bound_fields.len(), 1);
        assert!((r.bound_fields[0].confidence - 1.0).abs() < f32::EPSILON);
        // §7.5 step 2: the exact match also emits the
        // `Binds(FieldRef → Field)` edge the joiner persists.
        assert_eq!(field_binds.len(), 1, "exact match → one field bind");
        let fb = &field_binds[0];
        assert_eq!(fb.consumer.as_str(), fr_node.id);
        assert_eq!(fb.provider.as_str(), "orders:Field:openapi.yaml:id:10");
        assert!((fb.confidence - 1.0).abs() < f32::EPSILON);
        // §4.6: the endpoint-side bind is Heuristic here, so the
        // capped field edge is Heuristic too ("if either is").
        assert!(
            matches!(fb.provenance, EdgeProvenance::Heuristic { .. }),
            "Heuristic endpoint bind → Heuristic field bind"
        );
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
        let (out, schemaless, _unknown, field_binds) = resolve_field_refs(
            std::slice::from_ref(&fr_node),
            &reads_from,
            &binds,
            &schemas,
        );
        assert!(schemaless.is_empty());
        let r = out.values().next().unwrap();
        assert_eq!(r.bound_fields.len(), 1);
        assert!((r.bound_fields[0].confidence - 0.6).abs() < f32::EPSILON);
        // §7.5 step 3: unique suffix → one persisted-shape
        // `Binds(FieldRef → Field)` at the field_suffix confidence.
        assert_eq!(field_binds.len(), 1, "unique suffix → one field bind");
        let fb = &field_binds[0];
        assert_eq!(
            fb.provider.as_str(),
            "orders:Field:openapi.yaml:customer.id:5"
        );
        assert!((fb.confidence - 0.6).abs() < f32::EPSILON);
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
        let (out, schemaless, _unknown, field_binds) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert!(schemaless.is_empty());
        let r = out.values().next().unwrap();
        assert_eq!(r.bound_fields.len(), 2, "two ambiguous suffix matches");
        for b in &r.bound_fields {
            assert!((b.confidence - 0.3).abs() < f32::EPSILON);
        }
        // §7.5 step 3: ambiguous → one `Binds` edge per candidate,
        // each at the ambiguous_field confidence.
        assert_eq!(field_binds.len(), 2, "two ambiguous field binds");
        for fb in &field_binds {
            assert!((fb.confidence - 0.3).abs() < f32::EPSILON);
            // Both sides Heuristic at a 0.3 tie → the read side's
            // `ambiguous_field` detail is preserved.
            assert!(
                matches!(
                    &fb.provenance,
                    EdgeProvenance::Heuristic { detector, .. } if detector == "ambiguous_field"
                ),
                "expected Heuristic{{ambiguous_field}}, got {:?}",
                fb.provenance
            );
        }
        let providers: Vec<&str> = field_binds.iter().map(|b| b.provider.as_str()).collect();
        assert!(providers.contains(&"orders:Field:openapi.yaml:address.id:7"));
        assert!(providers.contains(&"orders:Field:openapi.yaml:customer.id:5"));
    }

    /// §4.6 + §7.5: the field-bind confidence is the **minimum** of
    /// the read's match confidence and the endpoint bind's, and the
    /// edge is `Heuristic` if either side is — with the detector
    /// detail of the weaker source preserved.
    #[test]
    fn field_bind_confidence_is_capped_by_the_endpoint_bind() {
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(
            endpoint.clone(),
            vec![field(
                "orders:Field:openapi.yaml:customer_id:10",
                "customer_id",
            )],
        );
        // An exact read (confidence 1.0) …
        let fr_node = node_field_ref("customer_id", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        // … against an endpoint bind weakened by rule 6's
        // `unbound_host` fallback to 0.6.
        let mut binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        binds[0].confidence = 0.6;
        binds[0].provenance = EdgeProvenance::Heuristic {
            detector: "unbound_host".into(),
            confidence: 0.6,
        };
        let (out, _schemaless, _unknown, field_binds) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);

        // The emitted Binds edge: min(1.0, 0.6) = 0.6, Heuristic
        // (the endpoint side is), detector from the weaker source.
        assert_eq!(field_binds.len(), 1);
        let fb = &field_binds[0];
        assert!((fb.confidence - 0.6).abs() < f32::EPSILON);
        match &fb.provenance {
            EdgeProvenance::Heuristic {
                detector,
                confidence,
            } => {
                assert_eq!(detector, "unbound_host");
                assert!((confidence - 0.6).abs() < f32::EPSILON);
            }
            other => panic!("expected Heuristic provenance, got {other:?}"),
        }
        // The resolution map carries the same cap.
        let r = out.values().next().unwrap();
        assert!((r.bound_fields[0].confidence - 0.6).abs() < f32::EPSILON);
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
        let (out, schemaless, unknown, _field_binds) =
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
        let (out, schemaless, _unknown, field_binds) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert_eq!(schemaless.len(), 1);
        assert!(
            field_binds.is_empty(),
            "schemaless endpoint → no field bind to emit"
        );
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
        let (out, _schemaless, unknown, field_binds) =
            resolve_field_refs(&[fr_node], &[], &[], &schemas);
        assert_eq!(unknown.len(), 1);
        assert!(field_binds.is_empty(), "no ReadsFrom → no field bind");
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
        let (out, _schemaless, unknown, field_binds) =
            resolve_field_refs(&[fr_node], &reads_from, &[], &schemas);
        assert_eq!(unknown.len(), 1);
        assert!(field_binds.is_empty(), "unbound call → no field bind");
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
            origin: crate::federation::contracts::model::FieldReadOrigin::FieldAccess,
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

    // ─── Task 6 (mutation-credibility plan): boundary fixtures ─────

    #[test]
    fn is_suffix_rejects_a_partial_byte_match() {
        // `fp[fp.len() - read.len()..] == read[..]` is a segment-
        // suffix check: a path that merely *starts* with the read
        // chain must not match, and a shorter fp never can.
        let fp: JsonPath = "orders.customer_id".parse().unwrap();
        let rc: JsonPath = "customer_id".parse().unwrap();
        assert!(is_suffix(&fp, &rc), "a trailing segment must match");
        let prefix: JsonPath = "customer_id_orders".parse().unwrap();
        assert!(
            !is_suffix(&prefix, &rc),
            "a prefix match must not be accepted as a suffix match"
        );
        let shorter: JsonPath = "cust".parse().unwrap();
        assert!(
            !is_suffix(&shorter, &rc),
            "a shorter fp cannot suffix-match"
        );
    }

    #[test]
    fn field_join_unique_suffix_guard_refuses_two_candidates() {
        // `suffix_matches.len() == 1` — two ResponseFields sharing a
        // path suffix must not silently bind to the first: §7.5
        // step 3 sends them down the ambiguous arm (one Binds each
        // at 0.3), never to `suffix_matches[0]`.
        let endpoint: EndpointId = (
            ServiceName("orders".into()),
            http_key(HttpMethod::Get, "/api/orders"),
        );
        let schemas = schemas_with(
            endpoint.clone(),
            vec![
                field("orders:Field:openapi.yaml:a.total:10", "a.total"),
                field("orders:Field:openapi.yaml:b.total:11", "b.total"),
            ],
        );
        let fr_node = node_field_ref("total", true);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        let (out, schemaless, _unknown, field_binds) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert!(schemaless.is_empty());
        let r = out.values().next().unwrap();
        assert!(!r.unknown, "ambiguous suffix binds are known, not unknown");
        assert_eq!(
            r.bound_fields.len(),
            2,
            "two fields sharing a suffix must both bind ambiguously, not pick the first: {:?}",
            r.bound_fields
        );
        assert_eq!(field_binds.len(), 2, "two ambiguous field binds");
        for fb in &field_binds {
            assert!((fb.confidence - 0.3).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn field_bind_tie_keeps_the_read_side_provenance() {
        // §4.6 tie-break (`r_conf <= endpoint.confidence`): when
        // read and endpoint bind are both Heuristic at equal
        // confidence, the weaker-source detail of the *read* side
        // wins. The fixture pins a genuine tie — both sides at 0.6.
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
        // Non-exact read → Heuristic { field_suffix, 0.6 }.
        let fr_node = node_field_ref("id", false);
        let reads_from: Vec<GraphEdge> = vec![edge(
            crate::schema::EdgeType::ReadsFrom,
            &fr_node.id,
            "billing:HttpClientCall:billing.py:get:1",
        )];
        let mut binds = vec![bind_edge(
            "billing:HttpClientCall:billing.py:get:1",
            &endpoint,
        )];
        binds[0].confidence = 0.6;
        binds[0].provenance = EdgeProvenance::Heuristic {
            detector: "unbound_host".into(),
            confidence: 0.6,
        };
        let (_out, _schemaless, _unknown, field_binds) =
            resolve_field_refs(&[fr_node], &reads_from, &binds, &schemas);
        assert_eq!(field_binds.len(), 1, "unique suffix must bind");
        let fb = &field_binds[0];
        assert!((fb.confidence - 0.6).abs() < f32::EPSILON, "min(0.6, 0.6)");
        match &fb.provenance {
            EdgeProvenance::Heuristic {
                detector,
                confidence,
            } => {
                assert_eq!(
                    detector, "field_suffix",
                    "a confidence tie must keep the read side's detector"
                );
                assert!((confidence - 0.6).abs() < f32::EPSILON);
            }
            other => panic!("expected Heuristic provenance, got {other:?}"),
        }
    }

    // ─── `collect_endpoint_schemas` negative branches ────────────────

    fn schema_graph_node(repo: &str, path: &str, name: &str, line: u32) -> GraphNode {
        let mut n = GraphNode::new_in(
            crate::schema::NodeType::Schema,
            name.to_string(),
            path.to_string(),
            &crate::schema::RepoNamespace::for_test(),
        );
        n.repo_id = Some(repo.to_string());
        n.id = format!("{repo}:Schema:{path}:{name}:{line}");
        n.line_start = Some(line);
        n.contract = Some(ContractFact::Schema {
            direction: Direction::Payload,
        });
        n
    }

    fn payload_field_node(repo: &str, path: &str, name: &str, line: u32) -> GraphNode {
        let mut n = GraphNode::new_in(
            crate::schema::NodeType::Field,
            name.to_string(),
            path.to_string(),
            &crate::schema::RepoNamespace::for_test(),
        );
        n.repo_id = Some(repo.to_string());
        n.id = format!("{repo}:Field:{path}:{name}:{line}");
        n.line_start = Some(line);
        n.contract = Some(ContractFact::Field(FieldMeta {
            ty: TypeDesc::String,
            required: true,
            nullable: false,
            enum_values: None,
        }));
        n
    }

    fn topic_provider_graph_node(
        repo: &str,
        node_name: &str,
        template: &str,
        line: u32,
    ) -> GraphNode {
        let mut n = GraphNode::new_in(
            crate::schema::NodeType::Topic,
            node_name.to_string(),
            "src/events.py".to_string(),
            &crate::schema::RepoNamespace::for_test(),
        );
        n.repo_id = Some(repo.to_string());
        n.id = format!("{repo}:Topic:src/events.py:{node_name}:{line}");
        n.line_start = Some(line);
        n.contract = Some(ContractFact::Provider(ProviderFact {
            method: HttpMethod::Any,
            template: template.to_string(),
            handler: None,
            operation_id: None,
            origin: ProviderOrigin::Code,
        }));
        n
    }

    fn http_provider_graph_node(repo: &str, template: &str, line: u32) -> GraphNode {
        let node_name = format!("http-post-{template}");
        let mut n = GraphNode::new_in(
            crate::schema::NodeType::HttpRoute,
            node_name.clone(),
            "src/api.py".to_string(),
            &crate::schema::RepoNamespace::for_test(),
        );
        n.repo_id = Some(repo.to_string());
        n.id = format!("{repo}:HttpRoute:src/api.py:{node_name}:{line}");
        n.line_start = Some(line);
        n.contract = Some(ContractFact::Provider(ProviderFact {
            method: HttpMethod::Post,
            template: template.to_string(),
            handler: None,
            operation_id: None,
            origin: ProviderOrigin::Code,
        }));
        n
    }

    fn assign_map(pairs: &[(String, &str)]) -> BTreeMap<String, ServiceName> {
        pairs
            .iter()
            .map(|(id, svc)| (id.clone(), ServiceName(svc.to_string())))
            .collect()
    }

    fn cfg_with_decl(
        services: &[&str],
        decl_repo: &str,
        decl_file: &str,
        decl_topic: &str,
    ) -> crate::federation::contracts::config::ContractFederationConfig {
        use crate::federation::contracts::config::{
            ContractFederationConfig, SchemaDecl, ServiceDecl,
        };
        ContractFederationConfig {
            services: services
                .iter()
                .map(|name| ServiceDecl {
                    name: (*name).into(),
                    repo: (*name).into(),
                    paths: vec![],
                    hosts: vec![],
                    env: vec![],
                    base_path: None,
                    route_prefixes: vec![],
                })
                .collect(),
            http_clients: vec![],
            generic_keys: vec![],
            schemas: vec![SchemaDecl {
                topic: decl_topic.into(),
                repo: decl_repo.into(),
                file: decl_file.into(),
            }],
            bindings: vec![],
            databases: vec![],
        }
    }

    fn cfg_without_decl(
        services: &[&str],
    ) -> crate::federation::contracts::config::ContractFederationConfig {
        use crate::federation::contracts::config::{ContractFederationConfig, ServiceDecl};
        ContractFederationConfig {
            services: services
                .iter()
                .map(|name| ServiceDecl {
                    name: (*name).into(),
                    repo: (*name).into(),
                    paths: vec![],
                    hosts: vec![],
                    env: vec![],
                    base_path: None,
                    route_prefixes: vec![],
                })
                .collect(),
            http_clients: vec![],
            generic_keys: vec![],
            schemas: vec![],
            bindings: vec![],
            databases: vec![],
        }
    }

    fn has_payload(out: &EndpointSchemas, key: &EndpointId) -> bool {
        out.by_endpoint
            .get(key)
            .map(|dirs| dirs.contains_key(&Direction::Payload))
            .unwrap_or(false)
    }

    fn topic_key(svc: &str, broker: &str, name: &str) -> EndpointId {
        (
            ServiceName(svc.to_string()),
            ContractKey::Topic {
                broker: broker.to_string(),
                name: name.to_string(),
            },
        )
    }

    #[test]
    fn schema_attach_drops_a_retracted_route() {
        // Defensive gate at `find(|n| n.id == route_id)`: the
        // `PayloadSchema` edge names a route that no longer exists
        // in `nodes` (the route was retracted before its schema
        // was — the caller still supplies its assignment). The
        // schema must be dropped; it must not attach through
        // whatever node happens to sit at the head of the list.
        let topic = topic_provider_graph_node("orders", "kafka/orders.events", "orders.events", 10);
        let schema = schema_graph_node("orders", "schemas/orders.avsc", "OrderEvent", 1);
        let fld = payload_field_node("orders", "schemas/orders.avsc", "order_id", 3);
        let has_field = GraphEdge::new(
            crate::schema::EdgeType::HasField,
            schema.id.clone(),
            fld.id.clone(),
        );
        let ghost = "orders:Topic:src/events.py:orders_events:99".to_string();
        let payload_edge = GraphEdge::new(
            crate::schema::EdgeType::PayloadSchema,
            ghost.clone(),
            schema.id.clone(),
        );
        let assignments = assign_map(&[
            (ghost, "orders"),
            (topic.id.clone(), "orders"),
            (schema.id.clone(), "orders"),
            (fld.id.clone(), "orders"),
        ]);
        let cfg = cfg_without_decl(&["orders"]);
        let out = collect_endpoint_schemas(
            &[topic, schema, fld],
            &[has_field, payload_edge],
            &assignments,
            &cfg,
        );
        assert!(
            out.by_endpoint.is_empty(),
            "a retracted route must drop its schema, not attach it to another node: {:?}",
            out.by_endpoint
        );
    }

    #[test]
    fn decl_loop_ignores_a_missing_schema_file() {
        // `n.path == decl.file` / `node_type == Schema && (…)`
        // false branches: `config.schemas` names
        // `schemas/orders.avsc` but no node carries that path. A
        // decoy Schema node with a different path must not be
        // picked up by the find, and nothing may attach.
        let decoy = schema_graph_node("orders", "schemas/decoy.avsc", "Decoy", 5);
        let decoy_field = payload_field_node("orders", "schemas/decoy.avsc", "decoy_id", 6);
        let topic = topic_provider_graph_node("orders", "kafka/orders.events", "orders.events", 10);
        let has_field = GraphEdge::new(
            crate::schema::EdgeType::HasField,
            decoy.id.clone(),
            decoy_field.id.clone(),
        );
        let assignments = assign_map(&[
            (decoy.id.clone(), "orders"),
            (decoy_field.id.clone(), "orders"),
            (topic.id.clone(), "orders"),
        ]);
        let cfg = cfg_with_decl(
            &["orders"],
            "orders",
            "schemas/orders.avsc",
            "orders.events",
        );
        let out = collect_endpoint_schemas(
            &[decoy, decoy_field, topic],
            &[has_field],
            &assignments,
            &cfg,
        );
        assert!(
            out.by_endpoint.is_empty(),
            "a missing declared schema file must attach nothing: {:?}",
            out.by_endpoint
        );
    }

    #[test]
    fn decl_loop_attaches_to_the_owning_topic_not_a_decoy() {
        // Primary find false branches: among two same-service
        // topics, only the one whose template equals `decl.topic`
        // may receive the schema — a decoy topic must not win the
        // find on type, assignment, or template inequality.
        let decoy = topic_provider_graph_node("orders", "kafka/decoy.events", "decoy.events", 5);
        let real = topic_provider_graph_node("orders", "kafka/orders.events", "orders.events", 10);
        let schema = schema_graph_node("orders", "schemas/orders.avsc", "OrderEvent", 1);
        let fld = payload_field_node("orders", "schemas/orders.avsc", "order_id", 3);
        let has_field = GraphEdge::new(
            crate::schema::EdgeType::HasField,
            schema.id.clone(),
            fld.id.clone(),
        );
        let assignments = assign_map(&[
            (decoy.id.clone(), "orders"),
            (real.id.clone(), "orders"),
            (schema.id.clone(), "orders"),
            (fld.id.clone(), "orders"),
        ]);
        let cfg = cfg_with_decl(
            &["orders"],
            "orders",
            "schemas/orders.avsc",
            "orders.events",
        );
        let out = collect_endpoint_schemas(
            &[decoy, real, schema, fld],
            &[has_field],
            &assignments,
            &cfg,
        );
        assert!(
            has_payload(&out, &topic_key("orders", "kafka", "orders.events")),
            "the owning topic endpoint must carry the declared schema: {:?}",
            out.by_endpoint.keys().collect::<Vec<_>>()
        );
        assert!(
            !has_payload(&out, &topic_key("orders", "kafka", "decoy.events")),
            "a same-service decoy topic must not receive the schema"
        );
    }

    #[test]
    fn decl_loop_primary_find_skips_a_sibling_service_topic() {
        // The primary find pins the assignment to the declaring
        // service: a sibling service publishing the same topic
        // name (on another broker) may not win the find.
        let mine = topic_provider_graph_node("orders", "kafka/orders.events", "orders.events", 10);
        let sibling =
            topic_provider_graph_node("reports", "rabbitmq/orders.events", "orders.events", 20);
        let schema = schema_graph_node("orders", "schemas/orders.avsc", "OrderEvent", 1);
        let fld = payload_field_node("orders", "schemas/orders.avsc", "order_id", 3);
        let has_field = GraphEdge::new(
            crate::schema::EdgeType::HasField,
            schema.id.clone(),
            fld.id.clone(),
        );
        let assignments = assign_map(&[
            (mine.id.clone(), "orders"),
            (sibling.id.clone(), "reports"),
            (schema.id.clone(), "orders"),
            (fld.id.clone(), "orders"),
        ]);
        let cfg = cfg_with_decl(
            &["orders", "reports"],
            "orders",
            "schemas/orders.avsc",
            "orders.events",
        );
        let out = collect_endpoint_schemas(
            &[mine, sibling, schema, fld],
            &[has_field],
            &assignments,
            &cfg,
        );
        assert!(
            has_payload(&out, &topic_key("orders", "kafka", "orders.events")),
            "the declaring service's own topic endpoint must carry the schema: {:?}",
            out.by_endpoint.keys().collect::<Vec<_>>()
        );
        assert!(
            !has_payload(&out, &topic_key("orders", "rabbitmq", "orders.events")),
            "the sibling-service topic endpoint must not receive the schema"
        );
    }

    #[test]
    fn decl_loop_fallback_finds_the_sibling_service_topic() {
        // Fallback find: no topic of `decl.topic` is assigned to
        // `orders` (the sibling publishes it), so the fallback must
        // find it — and must not be short-circuited by a same-
        // service decoy topic or by an HTTP route whose template
        // equals the topic name.
        let http = http_provider_graph_node("orders", "orders.events", 5);
        let decoy = topic_provider_graph_node("orders", "kafka/decoy.events", "decoy.events", 10);
        let sibling =
            topic_provider_graph_node("reports", "kafka/orders.events", "orders.events", 20);
        let schema = schema_graph_node("orders", "schemas/orders.avsc", "OrderEvent", 1);
        let fld = payload_field_node("orders", "schemas/orders.avsc", "order_id", 3);
        let has_field = GraphEdge::new(
            crate::schema::EdgeType::HasField,
            schema.id.clone(),
            fld.id.clone(),
        );
        let assignments = assign_map(&[
            (http.id.clone(), "orders"),
            (decoy.id.clone(), "orders"),
            (sibling.id.clone(), "reports"),
            (schema.id.clone(), "orders"),
            (fld.id.clone(), "orders"),
        ]);
        let cfg = cfg_with_decl(
            &["orders", "reports"],
            "orders",
            "schemas/orders.avsc",
            "orders.events",
        );
        let out = collect_endpoint_schemas(
            &[http, decoy, sibling, schema, fld],
            &[has_field],
            &assignments,
            &cfg,
        );
        assert!(
            has_payload(&out, &topic_key("orders", "kafka", "orders.events")),
            "the fallback must find the sibling-service topic: {:?}",
            out.by_endpoint.keys().collect::<Vec<_>>()
        );
        let http_key: EndpointId = (
            ServiceName("orders".into()),
            ContractKey::Http {
                method: MethodSpec::Known(HttpMethod::Post),
                template: "orders.events".into(),
            },
        );
        assert!(
            !has_payload(&out, &http_key),
            "an HTTP route sharing the topic name must not receive the payload schema"
        );
        assert!(
            !has_payload(&out, &topic_key("orders", "kafka", "decoy.events")),
            "a same-service decoy topic must not receive the schema"
        );
    }

    #[test]
    fn schema_lookup_is_order_independent_when_two_types_share_a_file() {
        // Two Schema nodes, same path, same repo, different names —
        // the multi-type-SDL collision (`schema.graphql` declaring
        // both `Order` and `Customer`). The pick must give the same
        // answer in either insertion order.
        let a = schema_graph_node("orders", "schema.graphql", "Order", 1);
        let b = schema_graph_node("orders", "schema.graphql", "Customer", 20);

        let nodes = vec![b.clone(), a.clone()];
        let picked = pick_schema_node(&nodes, "orders", "schema.graphql", Some("Order"))
            .expect("a schema node matches");
        assert_eq!(picked.name, "Order");

        let nodes = vec![a.clone(), b.clone()];
        let picked = pick_schema_node(&nodes, "orders", "schema.graphql", Some("Order"))
            .expect("a schema node matches");
        assert_eq!(picked.name, "Order");

        // Production calls the picker with no name hint (`SchemaDecl`
        // carries only topic/repo/file); the tie-break must still be
        // order-independent — lowest `line_start` wins.
        let nodes = vec![b.clone(), a.clone()];
        let picked = pick_schema_node(&nodes, "orders", "schema.graphql", None)
            .expect("a schema node matches");
        assert_eq!(picked.name, "Order");

        // Same line, different names, no name hint: the
        // lexicographically smallest `GlobalId` wins.
        let x = schema_graph_node("orders", "schema.graphql", "Alpha", 5);
        let z = schema_graph_node("orders", "schema.graphql", "Zeta", 5);
        let nodes = vec![z.clone(), x.clone()];
        let picked = pick_schema_node(&nodes, "orders", "schema.graphql", None)
            .expect("a schema node matches");
        assert_eq!(picked.name, "Alpha");
        let nodes = vec![x, z];
        let picked = pick_schema_node(&nodes, "orders", "schema.graphql", None)
            .expect("a schema node matches");
        assert_eq!(picked.name, "Alpha");
    }
}
