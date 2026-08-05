// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Explicit RPC allowlist for workload delegation credentials.
//!
//! Handler-level parent-child checks further constrain every sandbox and SSH
//! operation. Keep this list small: a delegation token must never become a
//! substitute for the supervisor's full gateway credential.

pub fn is_delegation_callable(path: &str) -> bool {
    matches!(
        path,
        "/openshell.v1.OpenShell/CreateSandbox"
            | "/openshell.v1.OpenShell/GetSandbox"
            | "/openshell.v1.OpenShell/DeleteSandbox"
            | "/openshell.v1.OpenShell/WatchSandbox"
            | "/openshell.v1.OpenShell/GetSandboxConfig"
            | "/openshell.v1.OpenShell/CreateSshSession"
            | "/openshell.v1.OpenShell/RevokeSshSession"
            | "/openshell.v1.OpenShell/ForwardTcp"
            | "/openshell.inference.v1.Inference/GetInferenceRoute"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_delegated_child_operations() {
        assert!(is_delegation_callable(
            "/openshell.v1.OpenShell/CreateSandbox"
        ));
        assert!(is_delegation_callable("/openshell.v1.OpenShell/ForwardTcp"));
        assert!(is_delegation_callable(
            "/openshell.inference.v1.Inference/GetInferenceRoute"
        ));
        assert!(!is_delegation_callable(
            "/openshell.v1.Inference/GetInferenceRoute"
        ));
        assert!(!is_delegation_callable(
            "/openshell.v1.OpenShell/RefreshSandboxToken"
        ));
        assert!(!is_delegation_callable(
            "/openshell.v1.Inference/GetInferenceBundle"
        ));
        assert!(!is_delegation_callable(
            "/openshell.v1.OpenShell/ListSandboxes"
        ));
    }
}
