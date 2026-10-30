// Exact upstream excerpts. See provenance.json; not a compilable crate.

// Upstream crates/openshell-sdk/src/raw.rs:21-22
pub use openshell_core::proto;
pub use openshell_core::proto::open_shell_client::OpenShellClient as GrpcClient;

// Upstream crates/openshell-sdk/src/raw.rs:38-44
/// Type alias for the gRPC client wrapped in the SDK's auth interceptor.
pub type AuthedGrpcClient = GrpcClient<
    tonic::service::interceptor::InterceptedService<
        tonic::transport::Channel,
        crate::EdgeAuthInterceptor,
    >,
>;

// Upstream crates/openshell-sdk/src/raw.rs:1-2
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
