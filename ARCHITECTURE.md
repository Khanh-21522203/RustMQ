# Rust-MQ Architecture Documentation

## Overview

Rust-MQ là message queue system tương thích Kafka protocol với hai chế độ hoạt động:
- **Single Broker Mode**: In-memory storage, phù hợp cho development
- **Multi-Broker Mode**: Distributed system với Raft consensus

## System Architecture

### High-Level Architecture

```mermaid
graph TB
    subgraph "Client Layer"
        Producer[Producer Client]
        Consumer[Consumer Client]
    end
    
    subgraph "Broker Layer"
        direction TB
        GrpcServer[gRPC Server<br/>KafkaBrokerServer]
        BrokerCore[Broker Core<br/>Event Loop]
        
        subgraph "Storage Layer"
            InMemory[InMemoryStorage<br/>Single Broker]
            MultiB[MultiBroker<br/>Raft-based]
        end
        
        subgraph "Raft Consensus"
            RaftStorage[SimpleRaftStorage]
            RaftNode[SimpleRaftNode]
            RaftState[BrokerData<br/>State Machine]
        end
    end
    
    Producer -->|Kafka Protocol| GrpcServer
    Consumer -->|Kafka Protocol| GrpcServer
    GrpcServer -->|mpsc channel| BrokerCore
    BrokerCore -->|BrokerStorage trait| InMemory
    BrokerCore -->|BrokerStorage trait| MultiB
    MultiB -->|Commands| RaftStorage
    RaftStorage -->|Manages| RaftNode
    RaftNode -->|Apply| RaftState
```

## Core Components

### 1. Client Layer

#### Producer
```rust
pub struct Producer {
    broker_address: String,
    topic: String,
    partition: i32,
    client: KafkaBrokerClient,
    batch_sender: mpsc::Sender<ProducerMessage>,
}
```

**Responsibilities:**
- Connect to broker via gRPC
- Batch messages for efficiency
- Send produce requests
- Handle acknowledgments

#### Consumer
```rust
pub struct Consumer {
    broker_address: String,
    topic: String,
    partition: i32,
    group_id: Option<String>,
    client: KafkaBrokerClient,
    offset: i64,
}
```

**Responsibilities:**
- Subscribe to topics
- Fetch messages continuously
- Commit offsets
- Handle group coordination

### 2. Broker Layer

#### KafkaBrokerServer (gRPC Server)

```mermaid
sequenceDiagram
    participant Client
    participant GrpcServer as KafkaBrokerServer
    participant Channel as mpsc Channel
    participant Core as BrokerCore
    participant Storage
    
    Client->>GrpcServer: Produce Request
    GrpcServer->>Channel: Send (Request, ResponseSender)
    Channel->>Core: Receive Request
    Core->>Storage: produce_message()
    Storage-->>Core: Result
    Core->>Channel: Send Response
    Channel-->>GrpcServer: Response
    GrpcServer-->>Client: Produce Response
```

**Key Functions:**
- `produce()`: Handle produce requests
- `fetch()`: Handle fetch requests
- `get_topic_metadata()`: Return topic/partition info
- `commit_offset()`: Commit consumer offsets

#### BrokerCore (Event Loop)

```rust
pub struct BrokerCore<S: BrokerStorage> {
    rpc_receive_channel: mpsc::Receiver<(BrokerGrpcRequest, mpsc::Sender<BrokerGrpcResponse>)>,
    storage: Arc<Mutex<S>>,
}
```

**Event Loop Flow:**

```mermaid
stateDiagram-v2
    [*] --> WaitForRequest
    WaitForRequest --> ReceiveRequest: recv()
    ReceiveRequest --> RouteRequest: match request type
    
    RouteRequest --> HandleProduce: ProduceRequest
    RouteRequest --> HandleFetch: FetchRequest
    RouteRequest --> HandleMetadata: MetadataRequest
    RouteRequest --> HandleOffset: OffsetRequest
    RouteRequest --> HandleGroup: GroupRequest
    
    HandleProduce --> CallStorage: storage.produce_message()
    HandleFetch --> CallStorage
    HandleMetadata --> CallStorage
    HandleOffset --> CallStorage
    HandleGroup --> CallStorage
    
    CallStorage --> SendResponse: Build response
    SendResponse --> WaitForRequest: Continue loop
```

**Responsibilities:**
- Route requests to appropriate handlers
- Coordinate with storage layer
- Send responses back through channels
- Single-threaded async event processing

### 3. Storage Layer

#### BrokerStorage Trait

```rust
#[async_trait]
pub trait BrokerStorage: Send + Sync {
    // Topic operations
    async fn get_topics(&self) -> Vec<String>;
    async fn get_topic_partitions(&self, topic: &str) -> Option<Vec<i32>>;
    
    // Message operations
    async fn produce_message(&mut self, topic: &str, partition: i32, message: Vec<u8>) -> Result<i64, String>;
    async fn fetch_messages(&self, topic: &str, partition: i32, offset: i64, max_bytes: i32) -> Result<(Vec<u8>, i64), String>;
    
    // Offset operations
    async fn commit_offset(&mut self, group: &str, topic: &str, partition: i32, offset: i64, metadata: String) -> Result<(), String>;
    async fn fetch_offset(&self, group: &str, topic: &str, partition: i32) -> Result<(i64, String), String>;
    
    // Group operations
    async fn join_group(&mut self, group_id: &str, member_id: &str, protocol_type: &str) -> Result<(i32, String, String, Vec<GroupMember>), String>;
    async fn leave_group(&mut self, group_id: &str, member_id: &str) -> Result<(), String>;
    async fn heartbeat(&mut self, group_id: &str, generation_id: i32, member_id: &str) -> Result<(), String>;
}
```

**Implementations:**

1. **InMemoryStorage** (Single Broker)
   - HashMap-based storage
   - No persistence
   - Fast for development

2. **MultiBroker** (Raft-based)
   - Distributed storage
   - Raft consensus
   - Leader election & replication

### 4. Raft Consensus Layer

#### Component Relationships

```mermaid
classDiagram
    class MultiBroker {
        -node_id: u64
        -raft_storage: Arc~SimpleRaftStorage~
        -local_storage: Arc~Mutex~LocalStorage~~
        +new(node_id, peers) MultiBroker
        +produce_message()
        +fetch_messages()
        +commit_offset()
    }
    
    class SimpleRaftStorage {
        -node: Arc~Mutex~SimpleRaftNode~~
        -tx: mpsc::Sender~BrokerCommand~
        +new(node_id, peers) SimpleRaftStorage
        +produce(topic, message)
        +commit_offset(group, offset)
        +get_messages(topic)
        +is_leader() bool
    }
    
    class SimpleRaftNode {
        -node_id: u64
        -raw_node: RawNode~MemStorage~
        -storage: Arc~Mutex~BrokerData~~
        -peers: HashMap~u64, String~
        +new(node_id, peers) SimpleRaftNode
        +propose(command)
        +tick()
        +step(msg)
        +ready()
        +advance(ready)
    }
    
    class BrokerData {
        +messages: HashMap~String, Vec~Vec~u8~~~
        +offsets: HashMap~String, i64~
    }
    
    class BrokerCommand {
        <<enumeration>>
        Produce
        CommitOffset
    }
    
    MultiBroker --> SimpleRaftStorage: uses
    SimpleRaftStorage --> SimpleRaftNode: manages
    SimpleRaftNode --> BrokerData: applies to
    SimpleRaftStorage --> BrokerCommand: sends
    SimpleRaftNode --> BrokerCommand: processes
    MultiBroker ..|> BrokerStorage: implements
```

#### Raft Consensus Flow

```mermaid
sequenceDiagram
    participant Client
    participant MultiBroker
    participant RaftStorage
    participant RaftNode1 as RaftNode (Leader)
    participant RaftNode2 as RaftNode (Follower)
    participant RaftNode3 as RaftNode (Follower)
    participant StateMachine as BrokerData
    
    Client->>MultiBroker: produce_message()
    MultiBroker->>MultiBroker: Check is_leader()
    
    alt Is Leader
        MultiBroker->>RaftStorage: produce(topic, message)
        RaftStorage->>RaftNode1: propose(BrokerCommand::Produce)
        
        Note over RaftNode1: Append to log
        
        par Replicate to Followers
            RaftNode1->>RaftNode2: AppendEntries RPC
            RaftNode1->>RaftNode3: AppendEntries RPC
        end
        
        RaftNode2-->>RaftNode1: ACK
        RaftNode3-->>RaftNode1: ACK
        
        Note over RaftNode1: Quorum reached<br/>Entry committed
        
        RaftNode1->>StateMachine: apply(BrokerCommand::Produce)
        StateMachine->>StateMachine: messages.push(message)
        
        RaftStorage-->>MultiBroker: Success
        MultiBroker-->>Client: Offset
    else Not Leader
        MultiBroker-->>Client: Error: Not leader
    end
```

#### Leader Election Process

```mermaid
stateDiagram-v2
    [*] --> Follower: Node starts
    
    Follower --> Candidate: Election timeout
    Candidate --> Leader: Wins election<br/>(majority votes)
    Candidate --> Follower: Loses election
    Leader --> Follower: Higher term discovered
    
    Follower --> Follower: Heartbeat received
    Leader --> Leader: Send heartbeats
    
    note right of Follower
        - Receive heartbeats
        - Vote for candidates
        - Redirect writes to leader
    end note
    
    note right of Candidate
        - Request votes
        - Vote for self
        - Random election timeout
    end note
    
    note right of Leader
        - Handle all writes
        - Send heartbeats
        - Replicate log entries
    end note
```

## Data Structures

### BrokerData (Raft State Machine)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokerData {
    // Topic -> Messages
    pub messages: HashMap<String, Vec<Vec<u8>>>,
    // Consumer Group -> Offset
    pub offsets: HashMap<String, i64>,
}
```

**State Transitions:**

```mermaid
graph LR
    A[BrokerCommand] -->|Propose| B[Raft Log]
    B -->|Replicate| C[Majority ACK]
    C -->|Commit| D[Apply to State]
    D -->|Update| E[BrokerData]
    E -->|Read| F[Client Response]
```

### Configuration

```rust
pub struct BrokerConfig {
    pub node_id: u64,
    pub api_addr: String,      // Client API address
    pub rpc_addr: String,       // Raft RPC address
    pub storage_path: String,   // Data directory
    pub cluster: ClusterConfig,
    pub raft: RaftConfig,
}
```

## Request Flow Examples

### Produce Message Flow

```mermaid
sequenceDiagram
    participant P as Producer
    participant GS as gRPC Server
    participant BC as BrokerCore
    participant MB as MultiBroker
    participant RS as RaftStorage
    participant RN as RaftNode
    
    P->>GS: ProduceRequest
    GS->>BC: (Request, ResponseChannel)
    BC->>MB: produce_message()
    
    alt Is Leader
        MB->>RS: produce(topic, msg)
        RS->>RN: propose(Produce)
        Note over RN: Raft consensus
        RN-->>RS: Committed
        RS-->>MB: Offset
        MB-->>BC: Result
        BC->>GS: Response
        GS-->>P: ProduceResponse
    else Not Leader
        MB-->>BC: Error(Not Leader)
        BC->>GS: Response
        GS-->>P: Error
    end
```

### Fetch Message Flow

```mermaid
sequenceDiagram
    participant C as Consumer
    participant GS as gRPC Server
    participant BC as BrokerCore
    participant MB as MultiBroker
    participant RS as RaftStorage
    participant BD as BrokerData
    
    C->>GS: FetchRequest(offset)
    GS->>BC: (Request, ResponseChannel)
    BC->>MB: fetch_messages(topic, offset)
    MB->>RS: get_messages(topic)
    RS->>BD: Read messages
    BD-->>RS: Messages
    RS-->>MB: Messages
    MB->>MB: Filter by offset
    MB-->>BC: (Messages, HighWatermark)
    BC->>GS: Response
    GS-->>C: FetchResponse
```

## Concurrency Model

### Single Broker Mode

```mermaid
graph TB
    subgraph "Main Thread"
        Server[gRPC Server<br/>Multiple Connections]
    end
    
    subgraph "Broker Thread"
        Core[BrokerCore<br/>Event Loop]
        Storage[InMemoryStorage<br/>Arc&lt;Mutex&gt;]
    end
    
    Server -->|mpsc channel| Core
    Core -->|Lock for each op| Storage
```

**Characteristics:**
- Single-threaded event loop
- Storage protected by Mutex
- Async/await for I/O

### Multi-Broker Mode

```mermaid
graph TB
    subgraph "Main Thread"
        Server[gRPC Server]
    end
    
    subgraph "Broker Thread"
        Core[BrokerCore]
        Multi[MultiBroker]
    end
    
    subgraph "Raft Thread"
        Ticker[Tick Loop<br/>100ms interval]
        RaftNode[RaftNode]
    end
    
    subgraph "Command Thread"
        CmdLoop[Command Loop<br/>Process proposals]
    end
    
    Server -->|mpsc| Core
    Core --> Multi
    Multi -->|Command channel| CmdLoop
    Ticker -->|tick| RaftNode
    CmdLoop -->|propose| RaftNode
    RaftNode -->|apply| State[BrokerData]
```

**Characteristics:**
- Multiple background tasks
- Lock-free communication via channels
- Raft tick loop for timeouts/elections
- Separate command processing loop

## Configuration Management

### Config File Structure

```yaml
# Node identity
node_id: 1
api_addr: "127.0.0.1:9092"    # Kafka API
rpc_addr: "127.0.0.1:19092"   # Raft RPC

# Data persistence
storage_path: "./data/broker-1"

# Cluster setup
cluster:
  initial_members:
    - node_id: 1
      api_addr: "127.0.0.1:9092"
      rpc_addr: "127.0.0.1:19092"
  bootstrap: true  # Only true for first node

# Raft tuning
raft:
  heartbeat_interval_ms: 1000
  election_timeout_min_ms: 3000
  election_timeout_max_ms: 6000
```

### Startup Flow

```mermaid
stateDiagram-v2
    [*] --> ParseConfig: Load YAML
    ParseConfig --> CheckMode: Parse successful
    
    CheckMode --> SingleBroker: No config file
    CheckMode --> MultiBroker: Config provided
    
    SingleBroker --> InitInMemory: Create InMemoryStorage
    InitInMemory --> StartServer: Start gRPC
    
    MultiBroker --> CreateRaft: Create Raft nodes
    CreateRaft --> Bootstrap: If bootstrap=true
    CreateRaft --> Join: If bootstrap=false
    
    Bootstrap --> InitCluster: Initialize as leader
    Join --> WaitLeader: Wait for existing cluster
    
    InitCluster --> StartServer
    WaitLeader --> StartServer
    
    StartServer --> [*]: Running
```

## Error Handling

### Error Types

```rust
// Storage errors
Err("Not leader. Please retry with leader node")
Err("Topic or partition not found")
Err("Unknown member")
Err("Group not found")

// Raft errors
RaftError::Store(...)        // Storage operation failed
RaftError::SnapshotOutOfDate // Snapshot conflict
```

### Retry Strategy

```mermaid
graph TD
    A[Client Request] --> B{Is Success?}
    B -->|Yes| C[Return Result]
    B -->|No| D{Error Type?}
    
    D -->|Not Leader| E[Find Leader]
    E --> F[Retry with Leader]
    F --> B
    
    D -->|Temporary| G[Backoff]
    G --> H[Retry Same Node]
    H --> B
    
    D -->|Permanent| I[Return Error]
```

## Performance Considerations

### Single Broker
- **Throughput**: High (no network overhead)
- **Latency**: Low (direct memory access)
- **Scalability**: Limited to single machine

### Multi-Broker
- **Throughput**: Medium (network + consensus overhead)
- **Latency**: Higher (Raft replication)
- **Scalability**: Horizontal (add more nodes)
- **Availability**: High (survives node failures)

### Optimization Techniques

1. **Batching**: Producer batches messages
2. **Async I/O**: Non-blocking operations
3. **Zero-copy**: Direct buffer passing where possible
4. **Pipeline**: Overlapping requests in flight

## Deployment Architecture

### 3-Node Cluster

```mermaid
graph TB
    subgraph "Node 1 (Leader)"
        N1A[Kafka API :9092]
        N1R[Raft RPC :19092]
        N1S[(Storage)]
    end
    
    subgraph "Node 2 (Follower)"
        N2A[Kafka API :9093]
        N2R[Raft RPC :19093]
        N2S[(Storage)]
    end
    
    subgraph "Node 3 (Follower)"
        N3A[Kafka API :9094]
        N3R[Raft RPC :19094]
        N3S[(Storage)]
    end
    
    Producer -->|Write| N1A
    Consumer1 -->|Read| N1A
    Consumer2 -->|Read| N2A
    
    N1R <-->|Replication| N2R
    N2R <-->|Replication| N3R
    N1R <-->|Replication| N3R
```

**Fault Tolerance:**
- Tolerates 1 node failure (quorum = 2/3)
- Automatic leader election on failure
- Data replicated to all nodes

## Future Enhancements

1. **Persistent Storage**: Replace MemStorage with RocksDB
2. **Snapshot Transfer**: Optimize catch-up for new nodes
3. **Dynamic Membership**: Add/remove nodes without restart
4. **Multi-Raft**: Partition data across multiple Raft groups
5. **Compression**: Reduce network bandwidth
6. **Metrics**: Prometheus integration for monitoring
