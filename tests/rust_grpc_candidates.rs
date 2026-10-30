use lain::server::sensors::grpc_consumer_sensor::{detect_rust_grpc_candidates, RustGrpcConfig};

fn candidates(src: &str) -> Vec<lain::server::sensors::grpc_consumer_sensor::RustGrpcCandidate> {
    let dir = tempfile::tempdir().unwrap();
    detect_rust_grpc_candidates(src, "sample.rs", &RustGrpcConfig::load(dir.path()).unwrap())
        .unwrap()
}

#[test]
fn constructors_aliases_fields_accessors_and_precise_multiline_sites() {
    let src = r#"
use api::orders_client::OrdersClient as Orders;
use api::{users_client::UsersClient as Users};
type Local = Orders<Channel>;
struct Gateway { orders: Local }
impl Gateway {
    fn raw(&mut self) -> Local { todo!() }
    fn run(&mut self) {
        self.orders.get(req);
        self.raw()
            .get(req);
    }
}
fn run() {
    let orders = Orders::new(channel);
    let users = Users::connect(addr).await?;
    orders.get(req); users.get(req);
}
"#;
    let calls = candidates(src);
    assert_eq!(calls.len(), 4, "{calls:#?}");
    assert_eq!(calls[0].receiver, "self.orders");
    assert_eq!(calls[1].receiver, "self.raw()");
    assert_eq!(calls[1].site.line, 11);
    assert_eq!(calls[2].generated_type, "api::orders_client::OrdersClient");
    assert_eq!(calls[3].generated_type, "api::users_client::UsersClient");
    assert_ne!(calls[2].site.start_byte, calls[3].site.start_byte);
    for call in calls {
        assert_eq!(&src[call.site.start_byte..call.site.end_byte], "get");
        assert!(!call.evidence.is_empty());
        assert!(call.evidence.iter().all(|e| e.site.path == "sample.rs"));
    }
}

#[test]
fn names_comments_strings_and_ambiguous_types_do_not_prove_identity() {
    for src in [
        "fn f() { let c = HttpClient::new(); c.create_sandbox(req); }",
        "fn f() { client.create_sandbox(req); }",
        "fn f() { let c = a::x_client::XClient::connect(addr); c.get(req); }",
        "fn f() { let s = \"client.get(req)\"; /* client.get(req) */ }",
        "use a::x_client::XClient as C; use b::x_client::XClient as C; fn f(c: C) { c.get(req); }",
        "type A = B; type B = A; fn f(c: A) { c.get(req); }",
        "use a::x_client::*; fn f(c: XClient) { c.get(req); }",
        "fn f() { let c = a::x_client::XClient::new(ch); let c = HttpClient::new(); c.get(req); }",
        "fn f(c: a::x_client::XClient<Ch>) {} fn g() { c.get(req); }",
        "fn f() { let mut c = a::x_client::XClient::new(ch); c = other; c.get(req); }",
        "fn f() { let mut c = a::x_client::XClient::new(ch); if cond { c = other; } c.get(req); }",
        "fn f() { let c = a::x_client::XClient::new(ch); closure(|c| c.get(req)); }",
        "fn f() { let c = a::x_client::XClient::new(ch); for c in others { c.get(req); } }",
        "fn f() { let c = a::x_client::XClient::new(ch); let (c,) = other; c.get(req); }",
        "fn f() { let c = a::x_client::XClient::new(ch); fn nested() { c.get(req); } }",
        "struct Plain; type C = Plain; fn f(c: C) { use api::x_client::XClient as Plain; c.get(req); }",
        "struct Plain; struct G { c: Plain } impl G { fn f(&self) { use api::x_client::XClient as Plain; self.c.get(req); } }",
        "struct Gateway {} mod other { struct Gateway {} impl Gateway { fn raw(&self) -> a::x_client::XClient<Ch> { todo!() } } } fn f(g: Gateway) { g.raw().get(req); }",
    ] {
        assert!(candidates(src).is_empty(), "false positive: {src}");
    }
}

#[test]
fn typed_parameters_and_same_named_methods_keep_receivers_separate() {
    let src = "fn f(a: api::a_client::AClient<Ch>, b: api::b_client::BClient<Ch>, c: HttpClient) { a.get(req); b.get(req); c.get(req); a.clone(); }";
    let calls = candidates(src);
    assert_eq!(calls.len(), 2, "{calls:#?}");
    assert_eq!(calls[0].receiver, "a");
    assert_eq!(calls[1].receiver, "b");
}

#[path = "use_cases/rust_grpc_candidates.rs"]
mod discovery_use_case;

#[test]
fn pinned_nvidia_candidates_require_explicit_cross_file_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let src = include_str!("fixtures/rust_grpc/nemo.rs");
    let baseline = RustGrpcConfig::load(dir.path()).unwrap();
    assert!(detect_rust_grpc_candidates(src, "nemo.rs", &baseline)
        .unwrap()
        .is_empty());
    let patterns = dir.path().join(".lain/patterns");
    std::fs::create_dir_all(&patterns).unwrap();
    std::fs::write(
        patterns.join("rust-grpc.yaml"),
        include_str!("fixtures/rust_grpc/rust-grpc.yaml"),
    )
    .unwrap();
    let config = RustGrpcConfig::load(dir.path()).unwrap();
    let calls = detect_rust_grpc_candidates(src, "nemo.rs", &config).unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|c| (c.method.as_str(), c.site.line))
            .collect::<Vec<_>>(),
        [("create_sandbox", 46), ("get_sandbox", 69)]
    );
    for call in &calls {
        assert_eq!(
            call.generated_type,
            "openshell_core::proto::open_shell_client::OpenShellClient"
        );
        assert!(call.evidence.iter().any(|e| e
            .description
            .contains("6648bd0c290efbc41ba131ee9831ee45cd431f94")));
        assert!(call
            .evidence
            .iter()
            .any(|e| e.description.contains("openshell_sdk::OpenShellClient")));
        let normalized_src = src.replace("\r\n", "\n").replace('\r', "\n");
        assert_eq!(
            &normalized_src[call.site.start_byte..call.site.end_byte],
            call.method
        );
    }
    let borrowed = src.replace(
        "client: OpenShellClient",
        "client: &'static OpenShellClient",
    );
    assert_eq!(
        detect_rust_grpc_candidates(&borrowed, "nemo.rs", &config)
            .unwrap()
            .len(),
        2
    );
    let unrelated = src.replace("client: OpenShellClient", "client: HttpClient");
    assert!(detect_rust_grpc_candidates(&unrelated, "nemo.rs", &config)
        .unwrap()
        .is_empty());
    // Exact pinned reexport + type alias also works locally, without the mapping.
    let raw = include_str!("fixtures/rust_grpc/openshell_raw.rs");
    let local = format!("{raw}\nfn f(c: AuthedGrpcClient) {{ c.create_sandbox(req); }}");
    let calls = detect_rust_grpc_candidates(&local, "raw.rs", &baseline).unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].generated_type,
        "openshell_core::proto::open_shell_client::OpenShellClient"
    );
}

#[test]
fn lifetime_qualified_references_keep_generated_identity() {
    let calls = candidates(
        r#"
use api::orders_client::OrdersClient as C;
struct Gateway { client: C<Channel> }
impl Gateway { fn raw<'a>(&'a self) -> &'a mut C<Channel> { todo!() } }
fn f<'a>(c: &'a C<Channel>, g: &'a mut Gateway, nested: &&'a C<Channel>) {
    c.get(req); g.client.get(req); g.raw().get(req); nested.get(req);
}
"#,
    );
    assert_eq!(calls.len(), 4, "{calls:#?}");
    assert!(calls
        .iter()
        .all(|c| c.generated_type == "api::orders_client::OrdersClient"));
}

#[test]
fn completed_pattern_scopes_do_not_hide_later_clients() {
    for earlier in [
        "for c in others {}",
        "match value { Some(c) => {}, _ => {} }",
        "if let Some(c) = value {}",
        "while let Some(c) = value {}",
    ] {
        let src =
            format!("fn f() {{ {earlier} let c = api::x_client::XClient::new(ch); c.get(req); }}");
        assert_eq!(candidates(&src).len(), 1, "missed call: {src}");
    }
    // Patterns only shadow the outer client inside their binding scope.
    for shadow in [
        "for c in others { c.get(req); }",
        "match value { Some(c) => { c.get(req); }, _ => {} }",
        "if let Some(c) = value { c.get(req); }",
        "while let Some(c) = value { c.get(req); }",
    ] {
        let src = format!("fn f() {{ let c = api::x_client::XClient::new(ch); {shadow} }}");
        assert!(candidates(&src).is_empty(), "false identity: {src}");
    }
}

#[test]
fn generic_type_parameters_shadow_imports_without_proving_client_identity() {
    for body in [
        "fn f<C: OtherClient>(c: C) { c.get(req); }",
        "struct Holder<C> { c: C } impl<C: OtherClient> Holder<C> { fn f(&self) { self.c.get(req); } }",
        "trait Wrapper<C: OtherClient> { fn f(c: C) { c.get(req); } }",
        "type Alias<C> = C; fn f(c: Alias<Plain>) { c.get(req); }",
    ] {
        let src = format!("use api::orders_client::OrdersClient as C; {body}");
        assert!(candidates(&src).is_empty(), "generic inherited imported identity: {src}");
    }
    let src = "use api::orders_client::OrdersClient as C; fn f<T>(c: C<Channel>, other: T) { c.get(req); }";
    assert_eq!(
        candidates(src).len(),
        1,
        "unrelated generic hid a known client"
    );
}

#[test]
fn local_nominal_items_do_not_inherit_generated_client_imports() {
    for item in [
        "struct Client;",
        "enum Client { Value }",
        "union Client { value: u8 }",
    ] {
        for body in [
            format!("fn f() {{ {item} let c = Client::new(); c.get(req); }}"),
            format!("fn f() {{ let c = Client::new(); c.get(req); {item} }}"),
            format!("fn f() {{ {item} fn g(c: Client) {{ c.get(req); }} }}"),
        ] {
            let src = format!("use api::orders_client::OrdersClient as Client; {body}");
            assert!(
                candidates(&src).is_empty(),
                "local type inherited imported identity: {src}"
            );
        }
    }
    let src = "use api::orders_client::OrdersClient as Client; fn f(c: Client<Channel>) { { struct Client; } c.get(req); }";
    assert_eq!(
        candidates(src).len(),
        1,
        "out-of-scope type hid a known client"
    );
}
