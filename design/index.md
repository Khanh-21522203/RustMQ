# RustMQ — Design Index

RustMQ is a Rust message-queue system that runs as a single binary in broker, producer, or consumer mode, with a Kafka-like gRPC API for data/control operations, a KRaft-style controller path for metadata replication, and pluggable Raft transport implementations (gRPC and SBE-over-TCP).

## Mental Map

┌─ Client Runtime ────────────────────────────────────────────────────────────┐  ┌─ Broker API/Data Path ─────────────────────────────────────────────────────┐
│ Owns: CLI mode boot, producer/consumer runtime loops, client config/cache   │  │ Owns: Kafka RPC ingress, topic metadata/create, produce/fetch/list-offsets │
│ Entry: src/main.rs                                                          │  │ Entry: src/broker/server/kafka_server.rs                                   │
│ Key:   src/client/producer.rs, src/client/consumer.rs, src/client/config.rs │  │ Key:   src/broker/server/rpc_router.rs, src/broker/server/core.rs          │
│ Uses:  Broker API/Data Path, Cluster & Controller                           │  │ Uses:  Cluster & Controller, Group Coordination                            │
└─────────────────────────────────────────────────────────────────────────────┘  └────────────────────────────────────────────────────────────────────────────┘

┌─ Group Coordination ────────────────────────────────────────────────────────┐  ┌─ Cluster & Controller ────────────────────────────────────────────────────────────┐
│ Owns: group membership, rebalance, assignment, heartbeat/leave semantics    │  │ Owns: node join/remove, controller command proposal/apply, liveness checks        │
│ Entry: src/broker/server/consumer_group.rs                                  │  │ Entry: src/main.rs (run_kraft_cluster)                                            │
│ Key:   src/broker/storage/traits.rs, src/api/kafka.proto                    │  │ Key:   src/broker/controller/raft_node.rs, src/broker/controller/state_machine.rs │
│ Uses:  Broker API/Data Path, Shared                                         │  │ Uses:  Replication & Transport, Shared                                            │
└─────────────────────────────────────────────────────────────────────────────┘  └───────────────────────────────────────────────────────────────────────────────────┘

┌─ Replication & Transport ───────────────────────────────────────────────────┐  ┌─ Shared ──────────────────────────────────────────────────────────────────────┐
│ Owns: partition log, ISR/HW reconciliation, follower fetch, Raft transport  │  │ Owns: broker config contract, API proto/contracts, envelope codec, ops refs   │
│ Entry: src/broker/kraft/broker.rs                                           │  │ Key:   src/broker/config.rs, src/api/kafka.proto, src/codec.rs, examples/*.rs │
│ Key:   src/broker/kraft/partition_log.rs, src/broker/kraft/isr_manager.rs,  │  │                                                                               │
│        src/broker/grpc/transport.rs, src/broker/sbe_tcp/transport.rs        │  │                                                                               │
│ Uses:  Cluster & Controller                                                 │  │                                                                               │
└─────────────────────────────────────────────────────────────────────────────┘  └───────────────────────────────────────────────────────────────────────────────┘

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

Validation and protocol assumptions are enforced in multiple layers: client config/topic validation in `src/client/config.rs`, broker-side topic and partition checks in `src/broker/storage/traits.rs`, and RPC error mapping in `src/broker/server/core.rs` against `src/api/kafka.proto`. Persistence is mixed by subsystem: controller metadata and peer state persist through sled-backed controller state, partition records persist in per-partition sled trees, while group coordination and committed offsets are still implemented as in-memory state in the current storage/coordinator paths. Operational visibility is primarily log-driven (`log`/`env_logger`) across CLI bootstrap, router dispatch, controller tasks, and transport loops; there is no first-class metrics/tracing schema in the current code. Redirect semantics are reused across data/control paths by returning `error_code = 6` and embedding leader address hints in string fields (for example in membership responses), which callers must parse consistently.

## Notes

