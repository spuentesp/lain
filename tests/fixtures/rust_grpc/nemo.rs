// Exact upstream excerpts. See provenance.json; not a compilable crate.

// Upstream environment-providers/openshell/src/lib.rs:17-21
use openshell_sdk::raw::proto;
use openshell_sdk::{
    AuthConfig, ClientConfig, DeleteOptions, DeletionOutcome, DeletionResult, ExecOptions,
    OpenShellClient, SandboxPhase, SandboxRef, SdkError, ServiceStatus,
};

// Upstream environment-providers/openshell/src/lib.rs:1967-1969
struct SdkGateway {
    client: OpenShellClient,
}

// Upstream environment-providers/openshell/src/lib.rs:1971-1972
#[async_trait]
impl Gateway for SdkGateway {

// Upstream environment-providers/openshell/src/lib.rs:1987-2052
    async fn create(
        &self,
        workspace: Option<&str>,
        request: SandboxCreate,
    ) -> Result<SandboxSnapshot, ProviderError> {
        let request = proto::CreateSandboxRequest {
            spec: Some(proto::SandboxSpec {
                environment: request.environment,
                template: Some(proto::SandboxTemplate {
                    image: request.image,
                    ..proto::SandboxTemplate::default()
                }),
                policy: request.policy,
                providers: request.providers,
                command: request.command,
                tty: false,
                ..proto::SandboxSpec::default()
            }),
            name: request.name.unwrap_or_default(),
            labels: request.labels,
            workspace_scope: Some(proto::workspace_selector(workspace.unwrap_or("default"))),
            ..proto::CreateSandboxRequest::default()
        };
        let response = self
            .client
            .raw_grpc()
            .create_sandbox(request)
            .await
            .map_err(|_error| {
                ProviderError::contract(grpc_error_code(_error.code()), "OpenShell create failed")
            })?
            .into_inner();
        let sandbox = response.sandbox.ok_or_else(|| {
            ProviderError::contract(
                "sdk_protocol",
                "OpenShell create response did not contain a sandbox",
            )
        })?;
        Ok(raw_sandbox_snapshot(sandbox))
    }

    async fn get(
        &self,
        workspace: Option<&str>,
        name: &str,
    ) -> Result<SandboxSnapshot, ProviderError> {
        let response = self
            .client
            .raw_grpc()
            .get_sandbox(proto::GetSandboxRequest {
                name: name.to_string(),
                workspace_scope: Some(proto::workspace_selector(workspace.unwrap_or("default"))),
            })
            .await
            .map_err(|_error| {
                ProviderError::contract(grpc_error_code(_error.code()), "OpenShell get failed")
            })?
            .into_inner();
        let sandbox = response.sandbox.ok_or_else(|| {
            ProviderError::contract(
                "sdk_protocol",
                "OpenShell get response did not contain a sandbox",
            )
        })?;
        Ok(raw_sandbox_snapshot(sandbox))
    }

// Upstream environment-providers/openshell/src/lib.rs:1-2
// SPDX-FileCopyrightText: Copyright (c) 2026, NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Fixture scaffolding: close the excerpted impl.
}
