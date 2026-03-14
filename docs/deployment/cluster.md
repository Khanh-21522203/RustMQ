# Cluster Deployment

A Rust-MQ cluster runs three or more broker nodes that replicate state using the Raft consensus algorithm. The cluster:

- Tolerates up to `(N-1)/2` node failures (1 failure in a 3-node cluster)
- Elects a new leader automatically if the current leader fails
- Persists state to disk so data survives restarts

## Requirements

- An odd number of nodes (3, 5, 7, …) for proper majority quorums
- All nodes must be able to reach each other on their `rpc_addr` ports
- Clocks should be reasonably synchronized (NTP recommended)

## Configuration

Each node gets its own config file. All nodes must list the same `initial_members`.

**Node 1** (`config/broker-1.yaml`):

```yaml
node_id: 1
api_addr: "127.0.0.1:9092"
rpc_addr: "127.0.0.1:19092"
storage_path: "./data/broker-1"

cluster:
  initial_members:
    - { node_id: 1, api_addr: "127.0.0.1:9092", rpc_addr: "127.0.0.1:19092" }
    - { node_id: 2, api_addr: "127.0.0.1:9093", rpc_addr: "127.0.0.1:19093" }
    - { node_id: 3, api_addr: "127.0.0.1:9094", rpc_addr: "127.0.0.1:19094" }
  bootstrap: true  # Only on node 1, and only for the initial cluster bring-up

raft:
  heartbeat_interval_ms: 1000
  election_timeout_min_ms: 3000
  election_timeout_max_ms: 6000
  snapshot_threshold: 10000

log_level: "info"
```

**Node 2** (`config/broker-2.yaml`): Same as above but `node_id: 2`, `api_addr/rpc_addr` for node 2, and `bootstrap: false` (or omit the field).

**Node 3** (`config/broker-3.yaml`): Same pattern.

> **Important**: `bootstrap: true` must be set on exactly one node for the initial cluster setup. Once the cluster is running, it should be removed or set to `false` — it is only needed during first-ever startup.

## Starting the Cluster

### Using the Helper Script

```bash
./scripts/start-cluster.sh
```

This starts all three brokers as background processes and waits for them to elect a leader.

To stop the cluster:

```bash
./scripts/stop-cluster.sh
```

### Manually

Start each node in a separate terminal (or as a background process):

```bash
# Terminal 1
cargo run --release -- --mode broker --config config/broker-1.yaml

# Terminal 2
cargo run --release -- --mode broker --config config/broker-2.yaml

# Terminal 3
cargo run --release -- --mode broker --config config/broker-3.yaml
```

The nodes will discover each other via `initial_members` and elect a leader. Look for a log line like:

```
[node 1] became leader for term 1
```

## Connecting Clients

Clients connect to the `api_addr` of any node. In cluster mode, only the leader accepts writes. If a client sends a produce request to a follower, it will receive a `NotLeaderForPartition` error and should retry against the leader.

A simple approach is to point clients at the current leader directly:

```yaml
broker:
  address: "http://127.0.0.1:9092"  # Node 1's api_addr
```

For transparent failover, put a load balancer (e.g., HAProxy, Nginx) in front of the cluster and have it route to the current leader based on health checks.

## Failover Testing

```bash
./scripts/test-failover.sh
```

This script sends messages, kills the leader, waits for a new election, and verifies messages are still readable.

## Storage Layout

Each node writes its persistent state to `storage_path`:

```
./data/broker-1/
├── raft-log/       # Raft log entries
└── snapshots/      # Periodic state machine snapshots
```

To wipe a node and rejoin the cluster cleanly, delete its `storage_path` directory before restarting.

## Cluster Operations

### Adding a Node

> Cluster membership changes are not yet automated. Planned for a future release.

### Replacing a Failed Node

If a node's data is corrupt or lost:

1. Stop the failed node's process (if still running).
2. Delete its `storage_path`.
3. Update the node's config if its address has changed.
4. Restart the node — it will re-sync state from the leader via snapshot.

The cluster can continue operating (with reduced fault tolerance) while the replacement node syncs.

## Raft Tuning

| Parameter | Guideline |
|---|---|
| `heartbeat_interval_ms` | Should be well below `election_timeout_min_ms`. Default 1000ms works for LAN. Increase for high-latency links. |
| `election_timeout_min/max_ms` | Should be 3–10× the heartbeat interval. Wider range reduces split votes. |
| `snapshot_threshold` | Lower values = more frequent snapshots (smaller log, more disk I/O). Higher values = larger log (fewer snapshots). |

## Monitoring

Key things to watch:

- **Leader stability**: Frequent re-elections indicate network instability or overloaded nodes.
- **Replication lag**: Followers falling behind the leader may indicate disk or network bottlenecks.
- **Log growth**: If `snapshot_threshold` is too high, the Raft log can grow unbounded.

Log output from each node includes the current Raft role (`leader`, `follower`, `candidate`) and term number.
