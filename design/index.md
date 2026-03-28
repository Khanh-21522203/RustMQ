# RustMQ - Design Index

RustMQ is a Rust-based message-queue system that runs as a single binary in broker, producer, or consumer mode. The runtime combines a Kafka-like client gRPC API, a KRaft-style metadata controller path, and pluggable Raft transports (gRPC or SBE-over-TCP), with client libraries and CLI tooling in the same repository.

## Feature Matrix

| Feature | Description | File | Status |
|---------|-------------|------|--------|
| CLI Mode Runtime | Command-line mode selection and runtime bootstrap for broker/producer/consumer | [cli-mode-runtime.md](cli-mode-runtime.md) | Stable |
| Broker Node Config Contract | Broker YAML schema and defaults for single-node, cluster, and join modes | [broker-node-config-contract.md](broker-node-config-contract.md) | Stable |
| Broker RPC Ingress and Dispatch | gRPC request ingress and channel-based dispatch into broker core handlers | [broker-rpc-ingress-dispatch.md](broker-rpc-ingress-dispatch.md) | Stable |
| Broker Topic and Data RPC Path | Topic metadata/create and produce/fetch/list-offset handler behavior | [broker-topic-data-path.md](broker-topic-data-path.md) | Stable |
| Consumer Group Coordination | Broker-side group membership, rebalance, assignment, and heartbeat state machine | [consumer-group-coordination.md](consumer-group-coordination.md) | In Progress |
| Consumer Offset State | Commit/fetch offset storage contracts and current persistence behavior | [consumer-offset-state.md](consumer-offset-state.md) | In Progress |
| Cluster Membership and Join | Add/remove node RPC handling and startup join/redirect workflow | [cluster-membership-and-join.md](cluster-membership-and-join.md) | In Progress |
| Controller Raft Command Pipeline | Metadata proposal, Raft commit/apply loop, and controller storage handle | [controller-raft-command-pipeline.md](controller-raft-command-pipeline.md) | In Progress |
| Controller Heartbeat and Failure Detection | Broker liveness heartbeat tracking and dead-broker failover assignment logic | [controller-heartbeat-failure-detection.md](controller-heartbeat-failure-detection.md) | In Progress |
| Partition Log and Acks-All Semantics | Sled-backed partition log model, HW gating, and `acks=all` wait behavior | [partition-log-and-acks-all.md](partition-log-and-acks-all.md) | Stable |
| ISR Reconciliation and HW Advancement | ISR computation and HW advancement from replica progress snapshots | [isr-reconciliation-and-hw.md](isr-reconciliation-and-hw.md) | In Progress |
| Follower Replication Fetch Loop | Background follower fetch task manager and replication trait contract | [follower-replication-fetch-loop.md](follower-replication-fetch-loop.md) | In Progress |
| Raft Transport gRPC | Tonic-based inter-controller Raft message transport | [raft-transport-grpc.md](raft-transport-grpc.md) | Stable |
| Raft Transport SBE TCP | Custom SBE-framed raw TCP transport for Raft messages | [raft-transport-sbe-tcp.md](raft-transport-sbe-tcp.md) | In Progress |
| Client App Config Contract | Producer/consumer client YAML schema, defaults, and validation | [client-app-config-contract.md](client-app-config-contract.md) | Stable |
| Producer Leader Routing and Batching | Producer partitioning, batching, leader discovery, and retry behavior | [producer-leader-routing-and-batching.md](producer-leader-routing-and-batching.md) | Stable |
| Consumer Group Polling and Offsets | Consumer runtime loops, rejoin flow, fetch path, and commit behavior | [consumer-group-polling-and-offsets.md](consumer-group-polling-and-offsets.md) | In Progress |
| Metadata Cache and RPC Client | Low-level gRPC client wrapper and topic-partition leader cache | [metadata-cache-and-rpc-client.md](metadata-cache-and-rpc-client.md) | Stable |
| Message Envelope Codec | Application payload envelope and binary/JSON codec helpers | [message-envelope-codec.md](message-envelope-codec.md) | Stable |
| Examples, Benchmarks, and Scripted Operations | Reference executables, criterion benches, and local cluster scripts | [examples-client-workflows-and-benchmarks.md](examples-client-workflows-and-benchmarks.md) | Stable |

## Cross-Cutting Concerns

Topic-name validation is duplicated on both client and broker sides (`src/client/config.rs` and `src/broker/storage/traits.rs`) to prevent invalid keys early and at storage boundaries. Error-code handling is partially centralized in `kafka.proto`, but several call paths rely on hardcoded values (`6` for NOT_LEADER redirects and `27` for rebalance handling), so compatibility between enum definitions and runtime mapping must be checked when changing protocol behavior. Persistence is split by subsystem: controller metadata and peer maps persist in sled (`controller_meta`, `peers`), partition records persist in per-partition sled trees, and some important runtime states (group membership and committed offsets in current paths) remain in-memory. Observability is log-driven throughout (`log`/`env_logger`) with no built-in metrics/tracing schema, so debugging typically starts from mode startup logs, router dispatch logs, and controller/transport warning paths.

## Notes
