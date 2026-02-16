# ADR 0001: Rust Core With Polyglot Connectors

- Status: Proposed
- Date: 2026-02-16
- Owners: OpenCrust maintainers

## Context

OpenCrust is a Rust rewrite of OpenClaw and aims for low-resource reliability and simple deployment.
At the same time, the connector ecosystem (Discord/Telegram/Slack/etc.) has mature libraries in TypeScript.
Requiring all connectors to be native Rust would slow adapter delivery and contributor growth.

We need an architecture that keeps core runtime behavior in Rust while allowing fast connector development.

## Decision

OpenCrust will use a **Rust core + polyglot connector** model:

1. Core orchestration, policy, and state remain in Rust.
2. Connectors may be implemented either:
   - in-process Rust channel implementations, or
   - out-of-process sidecars (for example TypeScript) using a versioned protocol.
3. Sidecar communication uses a strict JSON frame protocol (protocol versioned, capability-declared).
4. Connector trust and safety controls are enforced in the Rust core (auth, validation, size limits, allowlists).

## Initial Protocol Direction (v1)

1. Handshake frame with protocol version, connector metadata, and capabilities.
2. Message send/receive frames.
3. Status and health check frames.
4. Structured error frames.
5. Frame size limits and strict parsing.

The current protocol skeleton is defined in `crates/opencrust-channels/src/protocol.rs`.

## Consequences

### Positive

- Faster connector delivery by leveraging existing TS ecosystem.
- Lower barrier for external contributors.
- Rust core remains authoritative for runtime invariants and security.
- Cleaner path for commercial distribution (stable core, connector marketplace potential).

### Negative

- Additional protocol compatibility burden.
- More release coordination between core and sidecars.
- Operational complexity (managing sidecar processes and logs).

## Non-Goals

- Replacing the Rust core with TypeScript components.
- Defining a full plugin marketplace in this ADR.
- Finalizing IPC transport details (stdio vs local WebSocket) in this first draft.

## Follow-Up Work

1. Add ADR for sidecar transport and lifecycle management.
2. Implement connector supervisor in `opencrust-gateway` (start/stop/health/restart).
3. Add authentication requirements for sidecar connectors.
4. Build reference connector templates (Rust + TypeScript).
5. Add protocol conformance tests.
