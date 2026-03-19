# Transactional Workspace Exploration

## Goal

Explore adding a Fork/Explore/Commit style workspace to OpenShell so sandboxed
agent changes remain speculative until explicitly accepted.

This is not a replacement for the existing sandbox. It is an additional
filesystem persistence model layered on top of the current OpenShell runtime.

## Why This Fits OpenShell

OpenShell already has:

- a gateway that owns sandbox lifecycle and pod template generation
- a sandbox supervisor that already runs as root inside the sandbox
- filesystem policy and Landlock enforcement in the supervisor
- a policy model that distinguishes static filesystem controls from dynamic
  network/inference controls

That means the natural insertion point is not "a new policy rule" but a new
workspace mount model coordinated by the gateway and activated by the
supervisor.

## Current Constraints

### Existing shape

- `openshell-server` creates sandbox pods and side-loads `openshell-sandbox`
  into the agent container.
- `openshell-sandbox` applies Landlock and process/network setup before running
  the child command.
- Filesystem policy today is allowlist-oriented: read-only paths,
  read-write paths, and optional inclusion of the workdir.

### Important limitation

OpenShell's existing filesystem controls answer:

- what may be read
- what may be written

They do not answer:

- what changed
- whether those changes should persist
- how to discard speculative changes cleanly

That is the gap this exploration covers.

## Recommended Design

### Core model

For selected sandboxes, the gateway should provision a transactional workspace
root with three conceptual layers:

1. `base`
   The durable workspace content that survives sandbox restarts.
2. `upper`
   The sandbox-private writable layer where all modifications land.
3. `merged`
   The mount exposed to the agent process as its workdir.

Implementation target on Linux: kernel OverlayFS for privileged runtimes and
`fuse-overlayfs` for rootless runtimes.

The agent only sees `merged`. The base tree remains untouched until an explicit
commit operation.

### Control-plane ownership

The gateway should own the transactional workspace metadata:

- feature enabled/disabled for a sandbox
- workspace storage location
- branch lifecycle state
- commit/discard operations
- diff/summary metadata for the TUI or CLI

The supervisor should own the mount-time activation:

- mount `merged` from `base` + `upper`
- run the child process inside `merged`
- keep Landlock aligned with `merged`, not the host path

### Phase 1 scope

The narrowest useful first version is:

- sandbox starts with transactional workspace enabled
- all writes land in OverlayFS `upper`
- operator can inspect a diff summary
- operator can either:
  - commit: apply changes from `upper` into `base`
  - discard: delete `upper` and recreate a clean merged view

Do not couple the first version to human approval workflows, trust scoring, or
attestation. Those can come later if the basic mount and persistence model is
sound.

## Proposed API/Model Changes

### Sandbox spec

Add a transactional workspace section to `SandboxSpec` or `SandboxTemplate`
instead of overloading the existing filesystem policy.

Suggested shape:

```text
transactional_workspace:
  enabled: bool
  mode: disabled | overlayfs
  persist_base: bool
```

This should stay separate from:

- `filesystem_policy`: access control
- `network`: egress control
- `process`: privilege model

Reason: persistence semantics and access semantics are related but not the same.

### Gateway operations

New gateway operations would likely be required:

- `GetSandboxWorkspaceState`
- `CommitSandboxWorkspace`
- `DiscardSandboxWorkspace`
- optional `GetSandboxWorkspaceDiff`

These should be gateway-mediated, not direct in-sandbox commands, because the
gateway is the durable control plane and already fronts sandbox lifecycle.

## Where To Integrate

### Server

`crates/openshell-server/src/sandbox/mod.rs`

This is the right place to:

- inject extra volumes/mounts for transactional workspace state
- pass transactional workspace configuration to the supervisor
- define durable per-sandbox storage paths

### Sandbox supervisor

`crates/openshell-sandbox/src/main.rs`
`crates/openshell-sandbox/src/lib.rs`
`crates/openshell-sandbox/src/process.rs`

This is the right place to:

- prepare or attach the OverlayFS mount
- switch the effective workdir to `merged`
- ensure child processes only see the merged tree

### Policy layer

`crates/openshell-sandbox/src/policy.rs`

Do not force this into `FilesystemPolicy`. Add a sibling concept for workspace
mode if policy transport is needed.

## Key Risks

### 1. Nested mount/runtime assumptions

OpenShell currently runs inside a K3s-in-Docker architecture and the sandbox
supervisor itself already uses root privileges for namespace and policy setup.
OverlayFS inside that environment may have storage-driver and mount-propagation
constraints depending on the node/container filesystem backing.

This was validated on a Linux VM for both:

- privileged kernel OverlayFS
- rootless `fuse-overlayfs`

The remaining runtime question is how cleanly this maps into the full
OpenShell sandbox/container deployment path, not whether the workspace model
itself works.

### 2. Diff fidelity

OverlayFS gives a practical write-capture mechanism, but a good user-facing diff
experience still requires careful handling of:

- deletes and whiteouts
- renames
- metadata-only changes
- large binary files

Phase 1 can ship with a coarse summary instead of a perfect semantic diff.

### 3. Commit semantics

"Commit" should not mean "trust the sandbox to copy files itself". The commit
path should be gateway/supervisor controlled and ideally run with the sandbox
workload quiesced.

### 4. Portability

This feature is Linux-specific. OpenShell should keep non-transactional
workspaces as the default fallback.

## Recommendation

This feature is worth exploring in OpenShell.

This feature is worth continuing in OpenShell. The feasibility spike now
proves three important things:

1. OverlayFS mounts reliably inside the current OpenShell sandbox environment.
2. The supervisor can switch the effective workdir to a merged mount without
   breaking Landlock and existing process startup.
3. A simple discard/commit flow can be exposed cleanly through the gateway.

The next step is no longer "can this work?" It is productizing the gateway
control plane around commit/discard, diff presentation, and durable workspace
state.
