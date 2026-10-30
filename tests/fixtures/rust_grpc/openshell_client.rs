// Exact upstream excerpts. See provenance.json; not a compilable crate.

// Upstream crates/openshell-sdk/src/client.rs:15-15
use crate::raw::AuthedGrpcClient;

// Upstream crates/openshell-sdk/src/client.rs:37-46
pub struct OpenShellClient {
    channel: Channel,
    interceptor: EdgeAuthInterceptor,
    /// Drives OIDC token rotation. `None` when auth is static (edge token,
    /// anonymous, or an OIDC token with no refresher).
    token_source: Option<TokenSource>,
    /// Live bearer slot the interceptor reads; refreshed tokens are written
    /// here so rotation reaches in-flight requests. `None` for non-OIDC auth.
    bearer_slot: Option<BearerSlot>,
}

// Upstream crates/openshell-sdk/src/client.rs:48-48
impl OpenShellClient {

// Upstream crates/openshell-sdk/src/client.rs:99-104
    pub fn raw_grpc(&self) -> AuthedGrpcClient {
        proto::open_shell_client::OpenShellClient::with_interceptor(
            self.channel.clone(),
            self.interceptor.clone(),
        )
    }

// Upstream crates/openshell-sdk/src/client.rs:1-2
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Fixture scaffolding: close the excerpted impl.
}
