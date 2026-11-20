//! TypeScript / JavaScript gRPC stub calls.
//!
//! A client handle is recognised where it is built or typed, never by its
//! name alone:
//!
//! - `new FooClient(addr, creds)` (grpc-js, grpc-web generated clients)
//! - `FooClient.connect({ gateway })` / `.create(..)` / `.fromEnv(..)` (an SDK
//!   factory such as `@nvidia/openshell-sdk`)
//! - `createPromiseClient(FooService, transport)` / `createClient(FooDefinition, ..)`
//!   (connect-es, nice-grpc)
//! - a declaration typed `client: FooClient`
//!
//! Calls are `client.method(..)`, `this.client.method(..)` and
//! `client.raw.method(..)` (an SDK's generated surface). The method is
//! PascalCased (`getOrder` -> `GetOrder`) because proto methods are.
//!
//! A file is considered only when it names a gRPC/protobuf/SDK module, so an
//! `HttpClient` or `QueryClient` elsewhere is not read as a stub. Whether a
//! call binds is the joiner's decision: it needs a proto service of that name
//! and method.

use super::{host_parts_from_channel, ChannelTarget, GrpcStubCall};
use crate::federation::contracts::model::SourceSite;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// Client type bases that are not gRPC services.
const NOT_GRPC_CLIENTS: &[&str] = &[
    "Http",
    "Https",
    "Axios",
    "Fetch",
    "Query",
    "Apollo",
    "Redis",
    "Mongo",
    "S3",
    "Supabase",
    "Prisma",
    "Pg",
    "Mysql",
    "Sql",
    "Kafka",
    "Web",
    "Rest",
    "Graphql",
    "GraphQL",
    "Api",
    "Ws",
    "WebSocket",
    "Socket",
    "Stripe",
    "Github",
    "Octokit",
    "Slack",
    "Discord",
    "Telegram",
    "Docker",
    "Ssh",
    "Sftp",
    "Ftp",
    "Smtp",
    "Mqtt",
    "Nats",
    "Amqp",
    "Memcached",
    "Elastic",
    "Openai",
    "OpenAI",
    "Anthropic",
    "Mcp",
    "MCP",
    "Oauth",
    "OAuth",
    "Jwt",
    "Trpc",
    "Rpc",
];

/// Methods that are channel/lifecycle plumbing, not RPCs.
const NOT_RPC_METHODS: &[&str] = &[
    "close",
    "waitForReady",
    "getChannel",
    "makeUnaryRequest",
    "makeServerStreamRequest",
    "makeClientStreamRequest",
    "makeBidiStreamRequest",
    "on",
    "once",
    "off",
    "emit",
    "then",
    "catch",
    "finally",
    "toString",
    "constructor",
    "connect",
    "disconnect",
    "end",
    "destroy",
    "dispose",
    "cancel",
    "clone",
    "bind",
    "call",
    "apply",
    "unref",
    "ref",
    "setMaxListeners",
    "addListener",
    "removeListener",
    "removeAllListeners",
    "listeners",
    "valueOf",
];

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("typescript grpc regex"))
}

fn module_gate(content: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r#"(?i)["'`][@\w./-]*(?:grpc|connectrpc|bufbuild|protobuf|_pb|-sdk|/sdk|proto)[@\w./-]*["'`]"#,
    )
    .is_match(content)
}

/// The service a `FooClient` / `FooService` / `FooDefinition` name stands for.
fn service_of(name: &str, suffixes: &[&str]) -> Option<String> {
    let base = suffixes
        .iter()
        .find_map(|s| name.strip_suffix(s))
        .filter(|b| !b.is_empty())?;
    if NOT_GRPC_CLIENTS.contains(&base) {
        return None;
    }
    // An SDK narrows its client to a view (`OpenShellReadClient` over
    // `OpenShellClient`); the view's role word is not part of the service.
    // The joiner still requires the method to exist on the proto service.
    let service = ROLE_SUFFIXES
        .iter()
        .find_map(|s| base.strip_suffix(s).filter(|b| !b.is_empty()))
        .unwrap_or(base);
    Some(service.to_string())
}

/// Words an SDK appends to a service name for a narrowed client view.
const ROLE_SUFFIXES: &[&str] = &["ReadOnly", "Readonly", "Read", "Raw"];

const MODULE_KEYWORDS: &str = r"(?i)grpc|connectrpc|bufbuild|protobuf|_pb|-sdk|/sdk|proto";

/// What a file imports from gRPC-looking modules: the evidence that a
/// `FooClient` is a generated client and not some other `Client` type.
struct GrpcImports<'a> {
    content: &'a str,
    named: BTreeSet<String>,
    namespaces: BTreeSet<String>,
    specifiers: Vec<String>,
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

impl<'a> GrpcImports<'a> {
    fn new(content: &'a str) -> Self {
        static KEYWORD: OnceLock<Regex> = OnceLock::new();
        static NAMED: OnceLock<Regex> = OnceLock::new();
        static NAMESPACE: OnceLock<Regex> = OnceLock::new();
        static SPEC: OnceLock<Regex> = OnceLock::new();
        let keyword = re(&KEYWORD, MODULE_KEYWORDS);
        let named_re = re(
            &NAMED,
            r#"(?s)import\s+(?:type\s+)?\{([^}]*)\}\s*from\s*["']([^"']+)["']"#,
        );
        let namespace_re = re(
            &NAMESPACE,
            r#"(?:import\s+\*\s+as\s+(\w+)\s+from\s*["']([^"']+)["']|(?:const|let|var)\s+(\w+)\s*=\s*require\(\s*["']([^"']+)["']\s*\))"#,
        );
        let spec_re = re(&SPEC, r#"["'`]([@\w./-]+)["'`]"#);
        let mut named = BTreeSet::new();
        for cap in named_re.captures_iter(content) {
            if keyword.is_match(&cap[2]) {
                for item in cap[1].split(',') {
                    let item = item.trim().trim_start_matches("type ");
                    // `Foo as Bar` binds `Bar`.
                    if let Some(name) = item.rsplit(" as ").next().filter(|n| !n.is_empty()) {
                        named.insert(name.trim().to_string());
                    }
                }
            }
        }
        let mut namespaces = BTreeSet::new();
        for cap in namespace_re.captures_iter(content) {
            let (name, module) = match (cap.get(1), cap.get(2), cap.get(3), cap.get(4)) {
                (Some(n), Some(m), _, _) | (_, _, Some(n), Some(m)) => (n.as_str(), m.as_str()),
                _ => continue,
            };
            if keyword.is_match(module) {
                namespaces.insert(name.to_string());
            }
        }
        let specifiers = spec_re
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .filter(|spec| keyword.is_match(spec))
            .collect();
        Self {
            content,
            named,
            namespaces,
            specifiers,
        }
    }

    /// `client_type` (`OrdersClient`) of service `service` (`Orders`) is a
    /// generated gRPC client: imported from a gRPC module, reached through
    /// one, or named by one (`@nvidia/openshell-sdk` names `OpenShell`).
    fn vouches_for(&self, client_type: &str, service: &str) -> bool {
        if self.named.contains(client_type) {
            return true;
        }
        if self
            .namespaces
            .iter()
            .any(|ns| self.content.contains(&format!("{ns}.{client_type}")))
        {
            return true;
        }
        let service = normalize(service);
        !service.is_empty()
            && self
                .specifiers
                .iter()
                .any(|s| normalize(s).contains(&service))
    }
}

/// Variable (last identifier) -> service, from constructors, factories and
/// type annotations.
fn client_bindings(content: &str) -> BTreeMap<String, String> {
    static NEW: OnceLock<Regex> = OnceLock::new();
    static FACTORY: OnceLock<Regex> = OnceLock::new();
    static CREATE: OnceLock<Regex> = OnceLock::new();
    static TYPED: OnceLock<Regex> = OnceLock::new();
    let new = re(
        &NEW,
        r"(\w+)\s*(?::[^=;]+)?=\s*(?:await\s+)?new\s+(?:[\w.]+\.)?(\w+Client)\s*[(<]",
    );
    let factory = re(
        &FACTORY,
        r"(\w+)\s*(?::[^=;]+)?=\s*(?:await\s+)?(?:[\w.]+\.)?(\w+Client)\.(?:connect|create|fromEnv|forAddress|insecure)\s*\(",
    );
    let create = re(
        &CREATE,
        r"(\w+)\s*(?::[^=;]+)?=\s*(?:await\s+)?(?:createPromiseClient|createClient|createGrpcClient|createCallbackClient)\s*\(\s*(?:[\w.]+\.)?(\w+)",
    );
    let typed = re(
        &TYPED,
        r"\b(\w+)\s*\??:\s*(?:Promise<\s*)?(?:[\w.]+\.)?(\w+Client)\b",
    );
    let imports = GrpcImports::new(content);
    let mut out = BTreeMap::new();
    for line in content.lines() {
        for re in [new, factory, typed] {
            for cap in re.captures_iter(line) {
                if let Some(svc) = service_of(&cap[2], &["Client"]) {
                    if imports.vouches_for(&cap[2], &svc) {
                        out.insert(cap[1].to_string(), svc);
                    }
                }
            }
        }
        for cap in create.captures_iter(line) {
            if let Some(svc) = service_of(&cap[2], &["Service", "Definition", "Client"]) {
                out.insert(cap[1].to_string(), svc);
            }
        }
    }
    out
}

/// The host of a URL or `host:port` literal, ignoring `${..}` holes.
fn host_of(literal: &str) -> Option<String> {
    let mut s = literal.to_string();
    while let (Some(a), Some(b)) = (s.find("${"), s.find('}')) {
        if b < a {
            break;
        }
        s.replace_range(a..=b, "");
    }
    let s = s.trim();
    let s = s.split("://").last().unwrap_or(s);
    let host = s.split(['/', '?']).next().unwrap_or("");
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = if host.starts_with('[') {
        host
    } else {
        host.split(':').next().unwrap_or(host)
    };
    let host = host.trim();
    (!host.is_empty()
        && host
            .chars()
            .all(|c| c.is_alphanumeric() || "._-[]:".contains(c)))
    .then(|| host.to_string())
}

/// The dial address of the file: a literal first argument of a client
/// constructor or factory, or an address property (`gateway: "https://.."`).
fn channel_of(content: &str) -> Option<ChannelTarget> {
    static CTOR: OnceLock<Regex> = OnceLock::new();
    static PROP: OnceLock<Regex> = OnceLock::new();
    static ENV: OnceLock<Regex> = OnceLock::new();
    let ctor = re(
        &CTOR,
        r#"(?:new\s+(?:[\w.]+\.)?\w+Client|\w+Client\.(?:connect|create|forAddress))\s*\(\s*[`"']([^`"']+)[`"']"#,
    );
    let prop = re(
        &PROP,
        r#"\b(?:gateway|address|target|baseUrl|host|endpoint|grpcUrl|serverAddress)\s*:\s*[`"']([^`"']+)[`"']"#,
    );
    let env = re(
        &ENV,
        r#"process\.env(?:\.([A-Za-z_]\w*)|\[\s*["']([A-Za-z_]\w*)["']\s*\])"#,
    );
    for r in [ctor, prop] {
        if let Some(host) = r.captures(content).and_then(|c| host_of(&c[1])) {
            return Some(ChannelTarget::Literal(host));
        }
    }
    let vars: BTreeSet<String> = env
        .captures_iter(content)
        .filter_map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().to_string())
        })
        .collect();
    (!vars.is_empty()).then(|| ChannelTarget::Env(vars.into_iter().collect()))
}

fn pascal(method: &str) -> String {
    let mut chars = method.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

pub(super) fn detect_ts_stub_calls(content: &str, graph_path: &str, out: &mut Vec<GrpcStubCall>) {
    if !module_gate(content) {
        return;
    }
    let bindings = client_bindings(content);
    if bindings.is_empty() {
        return;
    }
    static CALL: OnceLock<Regex> = OnceLock::new();
    let call = re(
        &CALL,
        r"\b([A-Za-z_$][\w$]*)\s*(?:\??\.\s*raw)?\s*\??\.\s*([A-Za-z_$][\w$]*)\s*(?:<[^>()]*>)?\(",
    );
    let channel = channel_of(content);
    let (channel_target, host_part) = host_parts_from_channel(&channel);
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        // Comments and type-level lines are not calls.
        if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
            continue;
        }
        for cap in call.captures_iter(line) {
            let (Some(service), method) = (bindings.get(&cap[1]), &cap[2]) else {
                continue;
            };
            if NOT_RPC_METHODS.contains(&method) {
                continue;
            }
            // `new FooClient(` is a construction, never a call on a handle.
            let start = cap.get(0).map_or(0, |m| m.start());
            if line[..start].trim_end().ends_with("new") {
                continue;
            }
            out.push(GrpcStubCall {
                service: service.clone(),
                package: String::new(),
                method: pascal(method),
                channel_target: channel_target.clone(),
                channel_host_part: host_part.clone(),
                site: SourceSite {
                    path: graph_path.to_string(),
                    line: idx as u32 + 1,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(src: &str) -> Vec<GrpcStubCall> {
        let mut out = Vec::new();
        detect_ts_stub_calls(src, "src/x.ts", &mut out);
        out
    }

    #[test]
    fn sdk_factory_client_with_raw_surface_is_a_stub() {
        let src = "import('@nvidia/openshell-sdk');\nconst client = await sdk.OpenShellClient.connect({\n  gateway: `https://127.0.0.1:${String(port)}`,\n});\nawait client.raw.health({}, { signal });\nawait client.createSandbox(spec);\n";
        let c = calls(src);
        let methods: Vec<_> = c.iter().map(|x| x.method.as_str()).collect();
        assert_eq!(methods, ["Health", "CreateSandbox"], "{c:?}");
        assert!(c.iter().all(|x| x.service == "OpenShell"));
        assert_eq!(c[0].channel_target.as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn grpc_js_constructor_with_literal_address() {
        let src = "import * as grpc from '@grpc/grpc-js';\nimport { OrdersClient } from './gen/orders_grpc_pb';\nconst orders = new OrdersClient('orders:50051', grpc.credentials.createInsecure());\norders.getOrder(req, cb);\n";
        let c = calls(src);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(
            (c[0].service.as_str(), c[0].method.as_str()),
            ("Orders", "GetOrder")
        );
        assert_eq!(c[0].channel_target.as_deref(), Some("orders"));
    }

    #[test]
    fn connect_es_client_and_this_property() {
        let src = "import { createPromiseClient } from '@connectrpc/connect';\nimport { OrdersService } from './gen/orders_connect';\nclass A { private readonly orders = createPromiseClient(OrdersService, transport);\n async f() { await this.orders.listOrders({}); } }\n";
        let c = calls(src);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(
            (c[0].service.as_str(), c[0].method.as_str()),
            ("Orders", "ListOrders")
        );
    }

    #[test]
    fn type_annotation_binds_a_passed_client() {
        let src = "import type { BillingClient } from './gen/billing_grpc_pb';\nasync function f(client: BillingClient) { await client.charge(req); client.close(); }\n";
        let c = calls(src);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(
            (c[0].service.as_str(), c[0].method.as_str()),
            ("Billing", "Charge")
        );
    }

    #[test]
    fn http_and_query_clients_are_not_stubs() {
        let src = "import axios from 'axios';\nimport { QueryClient } from '@tanstack/query';\nconst http = new HttpClient('x');\nhttp.get(u);\nconst q: QueryClient = new QueryClient();\nq.invalidateQueries();\n";
        assert!(calls(src).is_empty());
    }

    #[test]
    fn file_without_a_grpc_module_is_ignored() {
        let src = "const c = new OrdersClient('orders:50051');\nc.getOrder(req);\n";
        assert!(calls(src).is_empty());
    }

    #[test]
    fn construction_and_lifecycle_calls_are_not_rpcs() {
        let src = "import * as grpc from '@grpc/grpc-js';\nconst orders = new OrdersClient('o:1', c);\norders.close();\norders.waitForReady(d, cb);\n";
        assert!(calls(src).is_empty());
    }

    #[test]
    fn env_dial_address_is_a_candidate_var() {
        let src = "import * as grpc from '@grpc/grpc-js';\nimport { OrdersClient } from './gen/orders_grpc_pb';\nconst orders = new OrdersClient(process.env.ORDERS_ADDR!, c);\norders.getOrder(req);\n";
        let c = calls(src);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            matches!(&c[0].channel_host_part, crate::federation::contracts::model::HostPart::Env(v) if v == &["ORDERS_ADDR".to_string()]),
            "{:?}",
            c[0].channel_host_part
        );
    }

    #[test]
    fn a_client_type_from_an_unrelated_module_is_not_vouched_for() {
        // The file mentions an SDK package elsewhere, but `SandboxClient` is the
        // app's own type: nothing ties it to a gRPC module.
        let src = "import { SandboxClient } from './sandbox';\nconst PKG = '@nvidia/other-sdk';\nasync function f(client: SandboxClient) { await client.exec(cmd); }\n";
        assert!(calls(src).is_empty());
    }

    #[test]
    fn service_named_by_the_sdk_module_is_vouched_for() {
        let src = "const PKG = '@nvidia/openshell-sdk';\nasync function f(client: OpenShellClient) { await client.listSandboxes({}); }\n";
        let c = calls(src);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(c[0].method, "ListSandboxes");
    }

    #[test]
    fn narrowed_client_view_names_the_underlying_service() {
        let src = "import { type OpenShellReadClient } from '@nvidia/openshell-sdk';\nasync function f(client: OpenShellReadClient) { await client.raw.getProviderProfile(req, opts); }\n";
        let c = calls(src);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!(
            (c[0].service.as_str(), c[0].method.as_str()),
            ("OpenShell", "GetProviderProfile")
        );
    }
}
