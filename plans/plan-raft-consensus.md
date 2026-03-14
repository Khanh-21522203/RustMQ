# Feature: Raft Consensus

## 1. Purpose

The Raft Consensus layer provides leader election and log replication across broker nodes, ensuring that the cluster has exactly one authoritative leader at all times and that all committed state is durably replicated to a majority of nodes before being acknowledged.

It wraps the `raft` crate's `RawNode` and drives the Raft protocol: ticking the state machine on a 100ms interval, proposing commands, applying committed log entries to `BrokerData` (the replicated state machine), and persisting Raft log entries via `MemStorage` (or a disk-backed implementation).

Without this layer, Rust-MQ is limited to single-node, volatile deployments. With it, the cluster tolerates up to `(N-1)/2` node failures (1 in a 3-node cluster) with automatic failover.

## 2. Responsibilities

- Wrap `raft::RawNode<MemStorage>` and manage the Raft tick loop (100ms interval per tick)
- Maintain `BrokerData`: the replicated state machine containing messages and committed offsets
- Serialize `BrokerCommand` variants with `bincode` and propose them to the Raft log
- Apply committed log entries to `BrokerData` via the state machine apply loop
- Expose `is_leader()` so `MultiBroker` can gate write operations
- Expose `propose_produce()` and `propose_commit_offset()` for the two write command types
- Handle `Ready` structs from `RawNode`: persist entries, send messages, advance state
- Manage inter-node message delivery via `RaftNetwork` (gRPC calls to peer `rpc_addr`s)
- Expose `SimpleRaftStorage` as the `BrokerStorage`-facing interface to `MultiBroker`

## 3. Non-Responsibilities

- Does not implement snapshot compaction (planned for a future milestone)
- Does not implement dynamic cluster membership changes (initial_members is fixed at startup)
- Does not replicate read requests (reads go to the leader's `BrokerData` directly)
- Does not expose the Raft state machine to the gRPC layer (only `MultiBroker` uses it)
- Does not handle network transport (that is `RaftNetwork`'s role)

## 4. Architecture Design

```
MultiBroker (implements BrokerStorage)
    |
    | propose_produce() / propose_commit_offset()
    v
+------------------------------------------+
|           SimpleRaftStorage              |
|  (async adapter: bridges tokio to raft)  |
|  propose_tx: mpsc::Sender<BrokerCommand> |
|  data: Arc<RwLock<BrokerData>>           |
+------------------------------------------+
    |
    | BrokerCommand (serialized via bincode)
    v
+------------------------------------------+
|            SimpleRaftNode                |
|                                          |
|  raw_node: RawNode<MemStorage>           |
|  tick loop: 100ms interval               |
|    → raw_node.tick()                     |
|    → drain propose_rx                    |
|    → process Ready:                      |
|        persist entries                   |
|        send messages via RaftNetwork     |
|        apply committed entries           |
|        advance()                         |
|  data: Arc<RwLock<BrokerData>>           |
+------------------------------------------+
    |
    | gRPC (raft.proto)
    v
+------------------------------------------+
|            RaftNetwork                   |
|  peers: HashMap<NodeId, RaftClient>      |
|  send(node_id, RaftMessage) → gRPC call  |
+------------------------------------------+
    |  inter-node gRPC over rpc_addr
    v
Peer RaftServer (receives and feeds to its RawNode)
```

### BrokerData (replicated state machine)

```
BrokerData:
  messages: HashMap<(topic: String, partition: i32), Vec<StoredMessage>>
  offsets:  HashMap<(group_id: String, topic: String, partition: i32), i64>
```

Applied atomically from committed Raft log entries.

## 5. Core Data Structures (Rust)

```rust
// src/broker/simple_raft.rs

use raft::{RawNode, Config as RaftConfig};
use raft::storage::MemStorage;
use bincode;

/// The replicated state machine. All fields are modified only by applying
/// committed Raft log entries in `SimpleRaftNode`'s apply loop.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct BrokerData {
    /// (topic, partition) → ordered message log; index == offset
    pub messages: HashMap<(String, i32), Vec<StoredMessage>>,
    /// (group_id, topic, partition) → committed consumer offset
    pub offsets:  HashMap<(String, String, i32), i64>,
}

/// Serialized commands written to the Raft log.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum BrokerCommand {
    Produce {
        topic:     String,
        partition: i32,
        key:       Option<Vec<u8>>,
        value:     Vec<u8>,
    },
    CommitOffset {
        group_id:  String,
        topic:     String,
        partition: i32,
        offset:    i64,
    },
}

/// Drives the Raft protocol for one broker node.
pub struct SimpleRaftNode {
    /// The underlying Raft state machine.
    raw_node: RawNode<MemStorage>,
    /// Shared replicated state. Written only by the apply loop; read by SimpleRaftStorage.
    data: Arc<RwLock<BrokerData>>,
    /// Receives proposed commands from SimpleRaftStorage.
    propose_rx: mpsc::UnboundedReceiver<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    /// Inter-node message sender.
    network: RaftNetwork,
    /// Raft configuration for this node.
    node_id: u64,
}

/// Async adapter that `MultiBroker` uses to interact with `SimpleRaftNode`.
/// Sends proposals over an mpsc channel and awaits their results.
#[derive(Clone)]
pub struct SimpleRaftStorage {
    propose_tx: mpsc::UnboundedSender<(BrokerCommand, oneshot::Sender<anyhow::Result<i64>>)>,
    data: Arc<RwLock<BrokerData>>,
    /// Raft node reference used to check is_leader().
    is_leader: Arc<AtomicBool>,
}
```

## 6. Public Interfaces

```rust
impl SimpleRaftNode {
    /// Create and start a Raft node with the given configuration.
    pub fn new(
        node_id: u64,
        peers: Vec<(u64, String)>,  // (node_id, rpc_addr)
        network: RaftNetwork,
    ) -> (Self, SimpleRaftStorage);

    /// Drive the Raft loop. Must be spawned on a Tokio task.
    ///   tokio::spawn(node.run());
    pub async fn run(mut self);
}

impl SimpleRaftStorage {
    /// Propose producing a message. Waits for Raft consensus; returns assigned offset.
    /// Returns Err if this node is not the leader.
    pub async fn propose_produce(
        &self,
        topic: String,
        partition: i32,
        key: Option<Vec<u8>>,
        value: Vec<u8>,
    ) -> anyhow::Result<i64>;

    /// Propose committing a consumer offset. Waits for Raft consensus.
    /// Returns Err if this node is not the leader.
    pub async fn propose_commit_offset(
        &self,
        group_id: String,
        topic: String,
        partition: i32,
        offset: i64,
    ) -> anyhow::Result<()>;

    /// Return true if this node is currently the Raft leader.
    pub fn is_leader(&self) -> bool;

    /// Read the current replicated state (for read-only queries).
    pub async fn read_data(&self) -> tokio::sync::RwLockReadGuard<'_, BrokerData>;
}
```

## 7. Internal Algorithms

### SimpleRaftNode::run (main Raft loop)

```
run():
  tick_interval = tokio::time::interval(100ms)

  loop:
    select!:
      _ = tick_interval.tick() →
        raw_node.tick()

      Some((cmd, reply_tx)) = propose_rx.recv() →
        if !raw_node.raft.state.is_leader():
          reply_tx.send(Err("not leader"))
          continue
        data = bincode::serialize(cmd)
        raw_node.propose(vec![], data)
        // Store reply_tx in pending map keyed by log index (assigned after Ready)
        pending.insert(next_index, reply_tx)

    // After each select arm: process Ready
    if raw_node.has_ready():
      ready = raw_node.ready()

      // 1. Persist entries
      MemStorage.wl().append(ready.entries())

      // 2. Apply snapshot if present
      if !ready.snapshot().is_empty():
        apply_snapshot(ready.snapshot())

      // 3. Send messages to peers
      for msg in ready.messages():
        network.send(msg.to, msg)

      // 4. Apply committed entries to BrokerData
      for entry in ready.committed_entries():
        if entry.data.is_empty(): continue  // config change or noop
        cmd: BrokerCommand = bincode::deserialize(entry.data)
        assigned_offset = apply_command(cmd)
        // Resolve pending reply
        if let Some(tx) = pending.remove(entry.index):
          tx.send(Ok(assigned_offset))

      // 5. Advance
      light_rd = raw_node.advance(ready)
      // 6. Apply light ready (committed entries from light_rd)
      raw_node.advance_apply()

      // 7. Update is_leader atomic
      is_leader.store(raw_node.raft.state == Leader, Ordering::SeqCst)
```

### apply_command

```
apply_command(cmd: BrokerCommand) -> i64:
  data = self.data.write().await

  match cmd:
    Produce { topic, partition, key, value } →
      log = data.messages.entry((topic, partition)).or_default()
      offset = log.len() as i64
      log.push(StoredMessage { offset, key, value })
      return offset

    CommitOffset { group_id, topic, partition, offset } →
      data.offsets.insert((group_id, topic, partition), offset)
      return offset
```

### propose_produce (in SimpleRaftStorage)

```
propose_produce(topic, partition, key, value):
  if !self.is_leader.load(Ordering::SeqCst):
    return Err("not leader for partition")
  (tx, rx) = oneshot::channel()
  self.propose_tx.send((BrokerCommand::Produce { topic, partition, key, value }, tx))?
  let result = rx.await??  // wait for Raft consensus
  return Ok(result)        // result is the assigned offset
```

## 8. Persistence Model

### Current (in-memory, MemStorage)

`MemStorage` holds the Raft log in RAM. State is lost if the process crashes. This is acceptable for the initial implementation — the focus is on correctness of the Raft protocol.

### Planned (disk-backed)

Replace `MemStorage` with a disk-backed log store (e.g., RocksDB or flat files) in `storage_path`. This enables recovery after crash:

```
$storage_path/
  raft-log/       # serialized Raft log entries (bincode)
  snapshots/      # periodic BrokerData snapshots (bincode)
```

On startup:
1. Load last snapshot → restore `BrokerData`
2. Replay log entries since snapshot → bring state up to date
3. Elect leader → resume normal operation

## 9. Concurrency Model

| Object | Primitive | Usage |
|---|---|---|
| `BrokerData` | `Arc<RwLock<BrokerData>>` | Written only by `apply_command` in the Raft loop task; read by `SimpleRaftStorage.read_data()` and `MultiBroker` |
| `propose_rx` | `mpsc::UnboundedReceiver` | Owned by `SimpleRaftNode`; drained in select loop |
| `propose_tx` | `mpsc::UnboundedSender` (cloned) | Held by `SimpleRaftStorage`; shared across `MultiBroker` clones |
| `is_leader` | `Arc<AtomicBool>` | Written by Raft loop; read by `propose_produce` |
| Pending replies | `HashMap<u64, oneshot::Sender>` | Owned entirely by the Raft loop task; no concurrent access |

**Single apply task**: All `BrokerData` mutations happen in the single Raft loop task. The `RwLock` write guard is held only during `apply_command` — never across the `tick` or `ready` processing steps.

## 10. Configuration

```rust
pub struct RaftNodeConfig {
    /// This node's ID (must be unique and match the entry in initial_members).
    pub node_id: u64,
    /// Addresses of all cluster members including self.
    pub initial_members: Vec<(u64, String)>,  // (node_id, rpc_addr)
    /// Raft tick interval (milliseconds). Default: 100.
    pub tick_interval_ms: u64,
    /// Ticks before leader sends heartbeat. Default: 10 (= 1000ms).
    pub heartbeat_tick: usize,
    /// Ticks before follower starts election. Default: 30–60 (= 3000–6000ms).
    pub election_tick: usize,
    /// Directory for persistent Raft state. None = in-memory only.
    pub storage_path: Option<PathBuf>,
    /// Log entries between snapshots. Default: 10000.
    pub snapshot_threshold: u64,
}
```

## 11. Observability

- Leader election: `INFO` log when node becomes leader or loses leadership, with term number
- Propose: `DEBUG` log with command type and proposed log index
- Apply: `TRACE` log with entry index and command type
- `is_leader` check that returns false: `WARN` once per second (avoid log spam on steady-state followers)
- Raft message send failure: `WARN` with peer node_id and error
- Pending reply count: `TRACE` metric (number of in-flight proposals awaiting consensus)

## 12. Testing Strategy

**Unit tests** (single-node, `SingleMode` equivalent):
- `test_single_node_becomes_leader`: start one node alone; after tick loop, assert `is_leader() == true`
- `test_produce_command_applied`: propose `BrokerCommand::Produce`, await result; assert offset 0 returned and message in `BrokerData`
- `test_commit_offset_command_applied`: propose `BrokerCommand::CommitOffset`; assert offset stored in `BrokerData`
- `test_propose_rejects_on_follower`: `is_leader` is false; assert `propose_produce` returns Err

**Integration tests** (3-node in-process cluster via in-memory `RaftNetwork`):
- `test_leader_election`: start 3 nodes; assert exactly one `is_leader()` within 10 seconds
- `test_log_replication`: leader proposes message; assert all 3 nodes have the message in `BrokerData`
- `test_leader_failover`: stop leader; assert new leader elected within 10s; propose message to new leader succeeds
- `test_majority_required`: start 3 nodes; partition 2 (majority); assert they elect a leader and can commit; isolated node cannot commit

## 13. Open Questions

None.
