//! Sealed `ProtocolDispatch` trait + per-protocol impls.
//!
//! The joiner previously walked every consumer node with a
//! hand-coded `if let Some(ContractFact::TopicConsumer(_)) = ...`
//! / `if let Some(ContractFact::RpcConsumer(_))` /
//! `if let Some(ContractFact::GraphqlConsumer(_)) = ...` cascade
//! in `run_with_registry_and_env`. Each branch shared the same
//! shape: parse a `GlobalId`, look up the implicit service, call
//! the matching `resolve_X_consumer`, and insert the result into
//! the `consumers` map. The cascade was also open-coded in
//! `build_endpoints` (the symmetric provider side) — adding a
//! fourth protocol meant editing both call sites and adding a
//! new `ContractFact` arm in `model.rs`.
//!
//! This module replaces the consumer-side cascade with a sealed
//! [`ProtocolDispatch`] trait. Each protocol gets one impl; the
//! orchestrator iterates a `Vec<Box<dyn ProtocolDispatch>>` and
//! calls `matches` then `dispatch` instead of the if-let ladder.
//! Adding a new protocol (spec §8.1 explicitly contemplates
//! Thrift and Connect-RPC) is now a single new dispatch entry
//! in the Vec plus a `ContractFact` arm — no edits to the
//! orchestrating for-loop.

use std::collections::BTreeMap;

use crate::federation::contracts::config::ContractFederationConfig;
use crate::federation::contracts::index::ConsumerResolution;
use crate::federation::contracts::joiner::{
    resolve_graphql_consumer, resolve_rpc_consumer, resolve_topic_consumer, EndpointProviderRecord,
};
use crate::federation::contracts::model::{ContractFact, ServiceName};
use crate::federation::repo_id::GlobalId;

mod sealed {
    pub trait Sealed {}
}

/// One consumer-side dispatch entry. The trait is sealed so the
/// only impls are the three protocol-specific dispatchers
/// (`TopicDispatch`, `RpcDispatch`, `GraphqlDispatch`) below.
/// Adding a new protocol is one new struct + two impls (`Sealed`
/// + `ProtocolDispatch`) plus a Vec entry at the call site.
pub trait ProtocolDispatch: sealed::Sealed {
    /// True iff this dispatch handles `fact` (i.e. the fact is
    /// the matching `ContractFact::XxxConsumer` variant).
    fn matches(&self, fact: &ContractFact) -> bool;

    /// Resolve the consumer to a [`ConsumerResolution`] and push
    /// any `Binds` edges onto `binds`. The caller has already
    /// validated `fact` via [`matches`]; the `dispatch` impl may
    /// `unreachable!()` on a non-matching fact (or the type
    /// system could enforce it — we keep the trait method
    /// signature simple for now).
    fn dispatch(
        &self,
        call_id: &GlobalId,
        own_service: &ServiceName,
        fact: &ContractFact,
        endpoints: &BTreeMap<
            (
                ServiceName,
                crate::federation::contracts::model::ContractKey,
            ),
            Vec<EndpointProviderRecord>,
        >,
        config: &ContractFederationConfig,
        binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
    ) -> ConsumerResolution;
}

// ─── TopicDispatch ───────────────────────────────────────────────────

/// §7.7 (stretch): resolves a `TopicConsumer` to a topic
/// producer's `(broker, name)` exact match. Topics have no
/// `HostPart` / template, only the `(broker, name)` pair.
pub struct TopicDispatch;

impl sealed::Sealed for TopicDispatch {}

impl ProtocolDispatch for TopicDispatch {
    fn matches(&self, fact: &ContractFact) -> bool {
        matches!(fact, ContractFact::TopicConsumer(_))
    }

    fn dispatch(
        &self,
        call_id: &GlobalId,
        own_service: &ServiceName,
        fact: &ContractFact,
        endpoints: &BTreeMap<
            (
                ServiceName,
                crate::federation::contracts::model::ContractKey,
            ),
            Vec<EndpointProviderRecord>,
        >,
        _config: &ContractFederationConfig,
        binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
    ) -> ConsumerResolution {
        let ContractFact::TopicConsumer(topic_consumer) = fact else {
            unreachable!("TopicDispatch::dispatch called with non-TopicConsumer fact")
        };
        resolve_topic_consumer(call_id, topic_consumer, own_service, endpoints, binds)
    }
}

// ─── RpcDispatch ──────────────────────────────────────────────────────

/// Phase E (spec §8.2): resolves an `RpcConsumer` (a generated
/// stub call site) to its `RpcProvider` by exact
/// `(package.Service, method)` match. The channel host is
/// resolved to candidate services first; an unknown channel
/// lands in `Unresolved { reason: RpcStubUnknown }`.
pub struct RpcDispatch;

impl sealed::Sealed for RpcDispatch {}

impl ProtocolDispatch for RpcDispatch {
    fn matches(&self, fact: &ContractFact) -> bool {
        matches!(fact, ContractFact::RpcConsumer(_))
    }

    fn dispatch(
        &self,
        call_id: &GlobalId,
        own_service: &ServiceName,
        fact: &ContractFact,
        endpoints: &BTreeMap<
            (
                ServiceName,
                crate::federation::contracts::model::ContractKey,
            ),
            Vec<EndpointProviderRecord>,
        >,
        config: &ContractFederationConfig,
        binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
    ) -> ConsumerResolution {
        let ContractFact::RpcConsumer(rpc_consumer) = fact else {
            unreachable!("RpcDispatch::dispatch called with non-RpcConsumer fact")
        };
        resolve_rpc_consumer(call_id, rpc_consumer, own_service, endpoints, config, binds)
    }
}

// ─── GraphqlDispatch ─────────────────────────────────────────────────

/// Phase E (spec §8.3): resolves a `GraphqlConsumer` to a
/// `GraphqlProvider` by exact `(op, field)` match. Multiple
/// providers on the same `(op, field)` (federation / gateway)
/// land in `Unresolved { reason: GraphqlNoOp }`.
pub struct GraphqlDispatch;

impl sealed::Sealed for GraphqlDispatch {}

impl ProtocolDispatch for GraphqlDispatch {
    fn matches(&self, fact: &ContractFact) -> bool {
        matches!(fact, ContractFact::GraphqlConsumer(_))
    }

    fn dispatch(
        &self,
        call_id: &GlobalId,
        own_service: &ServiceName,
        fact: &ContractFact,
        endpoints: &BTreeMap<
            (
                ServiceName,
                crate::federation::contracts::model::ContractKey,
            ),
            Vec<EndpointProviderRecord>,
        >,
        _config: &ContractFederationConfig,
        binds: &mut Vec<crate::federation::contracts::joiner::BindsEdge>,
    ) -> ConsumerResolution {
        let ContractFact::GraphqlConsumer(graphql_consumer) = fact else {
            unreachable!("GraphqlDispatch::dispatch called with non-GraphqlConsumer fact")
        };
        resolve_graphql_consumer(call_id, graphql_consumer, own_service, endpoints, binds)
    }
}

/// The default dispatch chain. The orchestrator iterates this
/// Vec in order — the first dispatch whose `matches` returns
/// true handles the node. Adding a new protocol is one Vec
/// entry plus the matching `ProtocolDispatch` impl.
pub fn default_dispatch_chain() -> Vec<Box<dyn ProtocolDispatch>> {
    vec![
        Box::new(TopicDispatch),
        Box::new(RpcDispatch),
        Box::new(GraphqlDispatch),
    ]
}
