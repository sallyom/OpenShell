# Transactional Workspace Summary

## TL;DR

We prototyped a transactional workspace for OpenShell so agents can edit files
in a speculative layer instead of writing directly to the real workspace. We
proved the model works on Linux with kernel OverlayFS in a privileged runtime:
existing files, new files, and directories all go into the upper layer while
the base stays unchanged.

Because kernel OverlayFS needs privilege, we also added a `fuse-overlayfs`
path for rootless use and introduced backend selection: `off`, `auto`,
`kernel`, and `fuse`. Both the privileged kernel path and the rootless FUSE
path have now been validated on Linux. The branch is still a feasibility spike,
not a full product feature yet; the next step would be gateway-managed
diff/commit/discard workflows.

## What We Explored

We prototyped a transactional workspace model for OpenShell sandboxes so an
agent can edit files in a speculative layer without mutating the real workspace
immediately.

User-facing goal:

- agent works in what looks like a normal writable directory
- base workspace remains unchanged while the agent runs
- changes can later be inspected, committed, or discarded

## What We Proved

### Kernel OverlayFS path

On Linux, a kernel OverlayFS-backed merged workdir works for this model:

- existing files can be modified through copy-up
- new files and directories can be created
- the base workspace remains unchanged
- cleanup of merged/work layers works

But this path requires a privileged runtime for the mount operation as currently
implemented. In practice, this means it is suitable for privileged Linux
sandbox/container environments, not ordinary rootless host execution.

### Rootless FUSE path

To support rootless users, we added a second backend path using
`fuse-overlayfs`, and validated it end-to-end on Fedora in a non-root runtime.

This keeps the same user model while avoiding the kernel OverlayFS mount
privilege requirement.

On Fedora rootless testing, the specific OpenShell integration issue turned out
to be `setpgid` during child launch. The spike now skips `setpgid` when the
active transactional backend is `fuse-overlayfs` in a non-root runtime.

## Backend Strategy

The prototype now supports these backend modes:

- `off` / `none`
- `auto`
- `kernel`
- `fuse`

Recommended behavior:

- `kernel`: require kernel OverlayFS and fail if unavailable
- `fuse`: require `fuse-overlayfs` and fail if unavailable
- `off`: disable transactional workspace entirely
- `auto`: try kernel first, then FUSE, then fall back to the normal workdir if
  neither is available

This makes transactional workspace a best-effort safety enhancement by default,
while still allowing strict opt-in behavior later.

## Product Interpretation

This feature is not “about OverlayFS”. It is about turning:

- “the agent edited your files directly”

into:

- “the agent proposed edits to your files”

That distinction matters for coding and file-mutating agents because it reduces
the impact of bad model behavior, prompt injection, and accidental destructive
writes.

## Current State Of The Branch

The branch contains:

- an experimental transactional workspace implementation in
  `openshell-sandbox`
- kernel OverlayFS support
- `fuse-overlayfs` support
- backend selection via CLI/env
- a smoke test script for Linux validation

This is still a feasibility spike, not a finished product feature, but the
backend plumbing and Linux smoke coverage are now working for both validated
paths.

## What Still Remains

The spike does **not** yet provide:

- commit/discard APIs through the OpenShell gateway
- diff presentation in CLI or TUI
- persistence metadata in sandbox spec/server APIs
- a “required vs best-effort” product policy layer

Those would be the natural Phase 2 items if the team wants to continue.

## Recommended Next Step

If the team wants to pursue this, the next implementation phase should be:

1. keep the current backend abstraction
2. add gateway-managed workspace state and explicit commit/discard operations
3. expose the feature in user-facing OpenShell workflows
4. decide whether `auto` should remain best-effort or grow a required/fail-closed mode
