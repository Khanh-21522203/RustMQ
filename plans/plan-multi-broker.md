# Feature: Multi-Broker

## 1. Purpose

Multi-Broker (`MultiBroker`) is the `BrokerStorage` implementation that enables fault-tolerant, replicated deployments. It wraps `SimpleRaftStorage` and implements the same `BrokerStorage` trait as `InMemoryStorage`, so `BrokerCore` requires no changes to support cluster mode.

Write operations (`produce_message`, `commit_offset`) are routed through Raft consensus: they block until a majority of nodes confirm the write. Read and metadata operations are served from the local replicated `BrokerData`, providing low-latency reads without a quorum round-trip. If this node is not the Raft leader, write operations return a `NotLeaderForPartition` error; clients should retry against the leader.

## 2. Responsibilities

- Implement `BrokerStorage` on top of `SimpleRaftStorage` for cluster deployments
- Gate all write operations (`produce_message`, `commit_offset`) behind an `is_leader()` check
- Return `ErrorCode::NotLeaderForPartition` immediately if the node is not the leader (no forwarding; clients retry)
- Route write commands through `SimpleRaftStorage.propose_produce()` and `SimpleRaftStorage.propose_commit_offset()`, which block until Raft consensus
- Serve read operations (`fetch_messages`, `earliest_offset`, `latest_offset`, `fetch_offset`) directly from the local `BrokerData` replica (eventually consistent; always consistent on the leader)
- Serve `get_topic_metadata()` with partition counts and the current leader's node ID
- Implement consumer group operations (`join_group`, `sync_group`, `heartbeat`, `leave_group`) using local in-memory state (group membership is not replicated via Raft; it rebuilds from heartbeats)
- Start `RaftNetwork` gRPC server on `rpc_addr` to receive inbound Raft peer messages

## 3. Non-Responsibilities

- Does not implement the Raft protocol (delegated to `SimpleRaftNode`)
- Does not forward requests to the leader (clients are responsible for retry/redirect)
- Does not replicate consumer group membership state via Raft (group state is ephemeral; only offsets are replicated)
- Does not perform leader discovery for clients; that is an operational concern (load balancer or DNS)
- Does not implement dynamic membership changes

## 4. Architecture Design

```
BrokerCore<MultiBroker>
    |
    | BrokerStorage trait calls
    v
+---------------------------------------------+
|                MultiBroker                  |
|                                             |
|  raft_storage: SimpleRaftStorage            |
|  groups: Arc<RwLock<GroupStore>>            |
|  node_id: u64                               |
|                                             |
|  Writes:                                    |
|    is_leader()? → if not: return Err        |
|    raft_storage.propose_*().await           |
|                                             |
|  Reads:                                     |
|    raft_storage.read_data().messages/offsets|
|                                             |
|  Metadata:                                  |
|    raft_storage.read_data().topic_partitions|
+---------------------------------------------+
    |
    | proposals
    v
SimpleRaftStorage → SimpleRaftNode (Raft loop)
    |
    | gRPC (raft.proto) on rpc_addr
    v
Peer nodes (RaftNetwork)
```

### Write vs Read path

```
Write (produce_message):
  if !raft_storage.is_leader():
    return Err(NotLeaderForPartition)
  raft_storage.propose_produce(topic, partition, key, value).await
    → blocks until majority ack
    → returns assigned offset
  return Ok(offset)

Read (fetch_messages):
  data = raft_storage.read_data().await   // RwLock read guard
  log = data.messages.get(&(topic, partition))
  return slice from offset...
  // No Raft round-trip needed; BrokerData is always up-to-date on leader
```

## 5. Core Data Structures (Rust)

```rust
// src/broker/multi_broker.rs

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// BrokerStorage implementation backed by Raft consensus.
pub struct MultiBroker {
    /// Async adapter to the Raft node.
    raft_storage: SimpleRaftStorage,
    /// This node's ID (used in metadata responses).
    node_id: u64,
    /// Known topic → partition count (populated from initial_members config
    /// and updated as new topics are implicitly created by produce operations).
    topic_partitions: Arc<RwLock<HashMap<String, i32>>>,
    /// Consumer group membership (not replicated; ephemeral).
    groups: Arc<RwLock<GroupStore>>,
}

/// Ephemeral consumer group state (mirrors the GroupState in InMemoryStorage).
struct GroupStore {
    groups: HashMap<String, GroupState>,
}

struct GroupState {
    generation_id: i32,
    leader_member_id: Option<String>,
    members: HashMap<String, GroupMember>,
    assignments: HashMap<String, Vec<u8>>,
}

/// Configuration for a multi-broker node.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrokerConfig {
    pub node_id: u64,
    pub api_addr: String,
    pub rpc_addr: String,
    pub storage_path: Option<PathBuf>,
    pub cluster: ClusterConfig,
    pub raft: RaftTuning,
    pub log_level: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClusterConfig {
    pub initial_members: Vec<ClusterMember>,
    pub bootstrap: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClusterMember {
    pub node_id: u64,
    pub api_addr: String,
    pub rpc_addr: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RaftTuning {
    pub heartbeat_interval_ms: u64,   // default: 1000
    pub election_timeout_min_ms: u64, // default: 3000
    pub election_timeout_max_ms: u64, // default: 6000
    pub snapshot_threshold: u64,      // default: 10000
}
```

## 6. Public Interfaces

```rust
impl MultiBroker {
    /// Build a MultiBroker, start the Raft node and RaftNetwork server.
    /// Returns the MultiBroker ready to be passed to BrokerCore.
    pub async fn new(config: BrokerConfig) -> anyhow::Result<Self>;
}

impl BrokerStorage for MultiBroker {
    // All 11 methods — see plan-broker-storage §6 for signatures
    // Write methods gate on is_leader(); read methods use BrokerData directly
}
```

## 7. Internal Algorithms

### MultiBroker::new

```
MultiBroker::new(config):
  // 1. Build RaftNetwork (gRPC clients to peers, server on rpc_addr)
  peers = config.cluster.initial_members
            .filter(|m| m.node_id != config.node_id)
            .map(|m| (m.node_id, m.rpc_addr))
            .collect()
  network = RaftNetwork::new(peers, config.rpc_addr).await?

  // 2. Build Raft tuning
  raft_config = RaftNodeConfig {
    node_id:            config.node_id,
    initial_members:    config.cluster.initial_members.map(|m| (m.node_id, m.rpc_addr)),
    heartbeat_tick:     (config.raft.heartbeat_interval_ms / 100) as usize,
    election_tick:      (config.raft.election_timeout_min_ms / 100) as usize,
    storage_path:       config.storage_path,
    snapshot_threshold: config.raft.snapshot_threshold,
    ...
  }

  // 3. Start SimpleRaftNode
  (raft_node, raft_storage) = SimpleRaftNode::new(raft_config, network)
  tokio::spawn(raft_node.run())

  // 4. Bootstrap if configured
  if config.cluster.bootstrap:
    // First node bootstraps the cluster by proposing a noop entry
    // (handled internally by RawNode on first tick as leader)
    ()

  Ok(MultiBroker { raft_storage, node_id: config.node_id, ... })
```

### produce_message

```
produce_message(topic, partition, key, value):
  if !self.raft_storage.is_leader():
    return Err(anyhow!("not leader for partition"))  // maps to ErrorCode::NotLeaderForPartition

  // Update local topic_partitions metadata if new partition seen
  {
    let mut tp = self.topic_partitions.write().await
    let current = tp.entry(topic.clone()).or_insert(0)
    if partition + 1 > *current: *current = partition + 1
  }

  offset = self.raft_storage
               .propose_produce(topic, partition, key, value)
               .await?
  return Ok(offset)
```

### fetch_messages

```
fetch_messages(topic, partition, offset, max_bytes):
  data = self.raft_storage.read_data().await
  log = data.messages.get(&(topic.to_owned(), partition))
  if log is None: return Ok(vec![])
  if offset < 0 or offset >= log.len() as i64: return Ok(vec![])

  result = []
  total_bytes = 0
  for msg in &log[offset as usize..]:
    size = msg.value.len() + msg.key.as_ref().map_or(0, |k| k.len())
    if total_bytes + size > max_bytes as usize and !result.is_empty(): break
    result.push(msg.clone())
    total_bytes += size
  return Ok(result)
```

### get_topic_metadata

```
get_topic_metadata(topics):
  data = self.raft_storage.read_data().await
  tp = self.topic_partitions.read().await
  leader_id = if self.raft_storage.is_leader(): self.node_id as i32 else: 0

  topics.iter().map(|name| {
    partition_count = tp.get(name).copied().unwrap_or(0)
    partitions = (0..partition_count).map(|p| PartitionMetadata {
      index: p,
      error_code: 0,
      leader_id,
    }).collect()
    TopicMetadata { name: name.clone(), error_code: 0, partitions }
  }).collect()
```

## 8. Persistence Model

### Replicated state (via Raft)

Messages and committed offsets in `BrokerData` are durably replicated once `propose_*` returns `Ok`. With `MemStorage`, this survives node restarts only if a majority of nodes remain available (the state is re-synced from peers on reconnect).

With disk-backed storage (see §plan-raft-consensus §8), data survives full cluster restarts via Raft log replay.

### Non-replicated state

Consumer group membership (`GroupStore`) is ephemeral. Members must rejoin and re-sync after broker restarts. This is consistent with Kafka's behavior: group state rebuilds from heartbeats.

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `raft_storage` | `SimpleRaftStorage` (Clone, `Arc` internally) | Shared across `BrokerCore` calls; thread-safe |
| `topic_partitions` | `Arc<RwLock<HashMap>>` | Read on metadata queries; written on first produce to a new partition |
| `groups` | `Arc<RwLock<GroupStore>>` | Written on join/leave/sync; read on heartbeat |

All locks use `tokio::sync::RwLock`. Lock guards are never held across `.await` points.

## 10. Configuration

See `BrokerConfig` in §5 above. Key defaults:

| Field | Default |
|---|---|
| `heartbeat_interval_ms` | 1000 |
| `election_timeout_min_ms` | 3000 |
| `election_timeout_max_ms` | 6000 |
| `snapshot_threshold` | 10000 |
| `bootstrap` | false |

## 11. Observability

- `MultiBroker::new`: `INFO` log with node_id, api_addr, rpc_addr, peer count
- Write rejected (not leader): `DEBUG` log (frequent in steady state; don't use WARN)
- First produce to new topic: `INFO` log with topic, partition count
- Raft leader status change (via callback from SimpleRaftNode): `INFO` log

## 12. Testing Strategy

**Unit tests** (mock `SimpleRaftStorage`):
- `test_produce_rejected_if_not_leader`: mock `is_leader() = false`; assert `produce_message` returns Err
- `test_produce_routes_through_raft`: mock `is_leader() = true`; assert `propose_produce` called with correct args
- `test_fetch_reads_from_broker_data`: populate mock `BrokerData`; assert `fetch_messages` returns expected messages
- `test_get_topic_metadata_leader_id`: assert leader_id field matches `node_id` when leader

**Integration tests** (3-node in-process cluster):
- `test_cluster_produce_and_fetch`: produce on leader; fetch on all 3 nodes; assert all return same data
- `test_non_leader_produce_rejected`: produce to follower; assert `NotLeaderForPartition` error
- `test_failover_produce_continues`: produce 10 messages to leader; stop leader; wait for new leader; produce 10 more; assert 20 messages on new leader
- `test_cluster_commit_offset_replicated`: commit offset on leader; read it back from all nodes

## 13. Open Questions

None.
