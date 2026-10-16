//! Key-compatibility rules shared by the confirmed-binding layers.
//!
//! A `repos.yaml#bindings[]` entry names a consumer key and a
//! provider key; the pair is well-formed exactly when
//! `check_binding` (§12) would accept the equivalent
//! (consumer, endpoint) proposal. `mcp/contract_tools/contracts.rs`
//! uses the same predicates for its `method_match` /
//! `template_match` verdicts — one implementation, this module.

use crate::federation::contracts::model::{ContractKey, HttpMethod, MethodSpec};
use crate::federation::contracts::route_match::match_route;

/// `check_binding`'s method rule: known methods must agree, unless
/// either side is `ANY`; `Unknown` (a call whose method the sensor
/// could not determine) rescues anything.
pub fn methods_compatible(cm: &MethodSpec, pm: &MethodSpec) -> bool {
    match (cm, pm) {
        (MethodSpec::Known(a), MethodSpec::Known(b)) => {
            a == b || matches!(b, HttpMethod::Any) || matches!(a, HttpMethod::Any)
        }
        (MethodSpec::Unknown, _) | (_, MethodSpec::Unknown) => true,
    }
}

/// `check_binding`'s template rule: a missing consumer template is
/// a pattern match; otherwise the two templates must match under
/// `match_route` (exact, pattern, or prefix-stripped — all fine;
/// only "no match" is not). The label is the wire-form verdict.
pub fn templates_compatible(consumer: Option<&str>, provider: &str) -> (bool, &'static str) {
    let Some(c) = consumer else {
        return (true, "pattern");
    };
    let r = match_route(
        MethodSpec::Known(HttpMethod::Any),
        c,
        HttpMethod::Any,
        provider,
    );
    if r.is_match() {
        (true, "exact")
    } else {
        (false, "none")
    }
}

/// Can a confirmed binding with `consumer` key pin a call site to
/// the endpoint named by `provider` key? The rules are
/// `check_binding`'s (§12) for a proposed (consumer, endpoint)
/// pair:
///
/// 1. both keys the same `ContractKey` variant (cross-kind pairs —
///    e.g. `graphql:query:product` → `http:POST /graphql` — are
///    ill-formed);
/// 2. HTTP pair: `methods_compatible` AND `templates_compatible`
///    (a same-kind template bridge like `GET /v1/api/orders/{}` →
///    `GET /api/orders/{}` is supported — §15.2 scenario 10);
/// 3. GraphQL pair: same `op` and same `field` (field bridging is
///    not a thing).
///
/// Any other same-kind pair (outside the `bindings` key grammar)
/// must agree exactly; cross-kind pairs never do.
pub fn keys_compatible(consumer: &ContractKey, provider: &ContractKey) -> bool {
    match (consumer, provider) {
        (
            ContractKey::Http {
                method: cm,
                template: ct,
            },
            ContractKey::Http {
                method: pm,
                template: pt,
            },
        ) => methods_compatible(cm, pm) && templates_compatible(Some(ct), pt).0,
        (
            ContractKey::Graphql { op: cop, field: cf },
            ContractKey::Graphql { op: pop, field: pf },
        ) => cop == pop && cf == pf,
        _ => consumer == provider,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::federation::contracts::model::GraphqlOp;
    use std::str::FromStr;

    #[test]
    fn methods_compatible_handles_any() {
        let g = MethodSpec::Known(HttpMethod::Get);
        let a = MethodSpec::Known(HttpMethod::Any);
        let u = MethodSpec::Unknown;
        assert!(methods_compatible(&g, &a));
        assert!(methods_compatible(&a, &g));
        assert!(methods_compatible(&u, &g));
        let p = MethodSpec::Known(HttpMethod::Post);
        assert!(!methods_compatible(&g, &p));
    }

    #[test]
    fn templates_compatible_returns_exact_when_paths_match() {
        let (ok, label) = templates_compatible(Some("/api/orders/{}"), "/api/orders/{}");
        assert!(ok);
        assert_eq!(label, "exact");
    }

    #[test]
    fn templates_compatible_returns_none_on_mismatch() {
        let (ok, label) = templates_compatible(Some("/api/users/{}"), "/api/orders/{}");
        assert!(!ok);
        assert_eq!(label, "none");
    }

    fn http(method: HttpMethod, template: &str) -> ContractKey {
        ContractKey::Http {
            method: MethodSpec::Known(method),
            template: template.to_string(),
        }
    }

    fn gql(op: GraphqlOp, field: &str) -> ContractKey {
        ContractKey::Graphql {
            op,
            field: field.to_string(),
        }
    }

    #[test]
    fn keys_compatible_accepts_identical_http_keys() {
        assert!(keys_compatible(
            &http(HttpMethod::Post, "/api/orders"),
            &http(HttpMethod::Post, "/api/orders"),
        ));
    }

    #[test]
    fn keys_compatible_accepts_http_template_bridge() {
        // §15.2 scenario 10: the call goes through a gateway `/v1`
        // prefix, the endpoint does not.
        assert!(keys_compatible(
            &http(HttpMethod::Get, "/v1/api/orders/{}"),
            &http(HttpMethod::Get, "/api/orders/{}"),
        ));
    }

    #[test]
    fn keys_compatible_rejects_http_method_mismatch() {
        assert!(!keys_compatible(
            &http(HttpMethod::Get, "/api/orders"),
            &http(HttpMethod::Post, "/api/orders"),
        ));
    }

    #[test]
    fn keys_compatible_rejects_http_template_mismatch() {
        assert!(!keys_compatible(
            &http(HttpMethod::Get, "/api/users/{}"),
            &http(HttpMethod::Get, "/api/orders/{}"),
        ));
    }

    #[test]
    fn keys_compatible_accepts_same_graphql_pair() {
        assert!(keys_compatible(
            &gql(GraphqlOp::Query, "product"),
            &gql(GraphqlOp::Query, "product"),
        ));
    }

    #[test]
    fn keys_compatible_rejects_graphql_field_mismatch() {
        assert!(!keys_compatible(
            &gql(GraphqlOp::Query, "product"),
            &gql(GraphqlOp::Query, "order"),
        ));
    }

    #[test]
    fn keys_compatible_rejects_cross_kind_pairs() {
        assert!(!keys_compatible(
            &gql(GraphqlOp::Query, "product"),
            &http(HttpMethod::Post, "/graphql"),
        ));
        assert!(!keys_compatible(
            &http(HttpMethod::Post, "/graphql"),
            &gql(GraphqlOp::Query, "product"),
        ));
    }

    #[test]
    fn keys_compatible_rejects_cross_kind_pairs_via_from_str_forms() {
        // The wire forms the config carries parse to the same
        // cross-kind pair as above.
        let c = ContractKey::from_str("graphql:query:product").expect("parse consumer key");
        let p = ContractKey::from_str("http:POST /graphql").expect("parse provider key");
        assert!(!keys_compatible(&c, &p));
    }

    #[test]
    fn keys_compatible_other_same_kind_pairs_must_agree_exactly() {
        let t1 = ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.created".into(),
        };
        let t2 = ContractKey::Topic {
            broker: "kafka".into(),
            name: "orders.updated".into(),
        };
        assert!(keys_compatible(&t1, &t1));
        assert!(!keys_compatible(&t1, &t2));
    }
}
