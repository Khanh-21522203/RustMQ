use async_trait::async_trait;
use openraft::{
    error::{InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError},
    network::{RaftNetwork, RaftNetworkFactory, RPCOption},
    raft::{
        AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
        VoteRequest, VoteResponse,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{transport::Channel, Request, Response, Status};

use crate::broker::raft_types::{BrokerNode, BrokerNodeId, TypeConfig};

/// gRPC service definition for Raft communication
pub mod raft_service {
    tonic::include_proto!("raft");
}

use raft_service::{
    raft_client::RaftClient,
    raft_server::{Raft, RaftServer},
    RaftMessage, RaftReply,
};

/// Network implementation for Raft cluster communication
#[derive(Clone)]
pub struct BrokerRaftNetwork {
    /// Node ID of this broker
    node_id: BrokerNodeId,
    
    /// Connections to other nodes
    connections: Arc<RwLock<HashMap<BrokerNodeId, Channel>>>,
}

impl BrokerRaftNetwork {
    pub fn new(node_id: BrokerNodeId) -> Self {
        Self {
            node_id,
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    async fn get_connection(&self, target: BrokerNodeId, target_node: &BrokerNode) -> Result<Channel, NetworkError<BrokerNodeId>> {
        let mut connections = self.connections.write().await;
        
        if let Some(channel) = connections.get(&target) {
            return Ok(channel.clone());
        }
        
        // Create new connection
        let channel = Channel::from_shared(format!("http://{}", target_node.rpc_addr))
            .map_err(|e| NetworkError::new(&e))?
            .connect()
            .await
            .map_err(|e| NetworkError::new(&e))?;
        
        connections.insert(target, channel.clone());
        Ok(channel)
    }
    
    async fn send_rpc<Req, Resp>(
        &self,
        target: BrokerNodeId,
        target_node: &BrokerNode,
        rpc: &str,
        req: Req,
    ) -> Result<Resp, RPCError<BrokerNodeId, BrokerNode, RaftError<BrokerNodeId>>>
    where
        Req: Serialize,
        Resp: for<'a> Deserialize<'a>,
    {
        let channel = self.get_connection(target, target_node).await
            .map_err(|e| RPCError::Network(e))?;
        
        let mut client = RaftClient::new(channel);
        
        let serialized = serde_json::to_vec(&req)
            .map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        
        let request = RaftMessage {
            rpc_type: rpc.to_string(),
            data: serialized,
        };
        
        let response = client.send_raft(Request::new(request))
            .await
            .map_err(|e| RPCError::Network(NetworkError::new(&e)))?
            .into_inner();
        
        if !response.error.is_empty() {
            return Err(RPCError::RemoteError(RemoteError::new(
                target,
                target_node.clone(),
                RaftError::APIError(response.error),
            )));
        }
        
        let result = serde_json::from_slice(&response.data)
            .map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        
        Ok(result)
    }
}

#[async_trait]
impl RaftNetworkFactory<TypeConfig> for BrokerRaftNetwork {
    type Network = BrokerRaftNetworkConnection;

    async fn new_client(&mut self, target: BrokerNodeId, node: &BrokerNode) -> Self::Network {
        BrokerRaftNetworkConnection {
            target,
            target_node: node.clone(),
            network: self.clone(),
        }
    }
}

pub struct BrokerRaftNetworkConnection {
    target: BrokerNodeId,
    target_node: BrokerNode,
    network: BrokerRaftNetwork,
}

#[async_trait]
impl RaftNetwork<TypeConfig> for BrokerRaftNetworkConnection {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<BrokerNodeId>, RPCError<BrokerNodeId, BrokerNode, RaftError<BrokerNodeId>>> {
        self.network.send_rpc(self.target, &self.target_node, "append_entries", req).await
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<InstallSnapshotResponse<BrokerNodeId>, RPCError<BrokerNodeId, BrokerNode, InstallSnapshotError>> {
        self.network.send_rpc(self.target, &self.target_node, "install_snapshot", req).await
            .map_err(|e| match e {
                RPCError::RemoteError(remote_err) => {
                    RPCError::RemoteError(RemoteError::new(
                        remote_err.target,
                        remote_err.target_node,
                        InstallSnapshotError::from(remote_err.source.to_string()),
                    ))
                }
                other => other.map_source(|_| InstallSnapshotError::from("RPC Error")),
            })
    }

    async fn vote(
        &mut self,
        req: VoteRequest<BrokerNodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<BrokerNodeId>, RPCError<BrokerNodeId, BrokerNode, RaftError<BrokerNodeId>>> {
        self.network.send_rpc(self.target, &self.target_node, "vote", req).await
    }
}

/// gRPC server implementation for Raft
pub struct BrokerRaftService {
    raft: Arc<openraft::Raft<TypeConfig>>,
}

impl BrokerRaftService {
    pub fn new(raft: Arc<openraft::Raft<TypeConfig>>) -> Self {
        Self { raft }
    }
}

#[async_trait]
impl Raft for BrokerRaftService {
    async fn send_raft(&self, request: Request<RaftMessage>) -> Result<Response<RaftReply>, Status> {
        let msg = request.into_inner();
        
        let reply = match msg.rpc_type.as_str() {
            "append_entries" => {
                let req: AppendEntriesRequest<TypeConfig> = serde_json::from_slice(&msg.data)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                
                match self.raft.append_entries(req).await {
                    Ok(resp) => RaftReply {
                        data: serde_json::to_vec(&resp).unwrap(),
                        error: String::new(),
                    },
                    Err(e) => RaftReply {
                        data: Vec::new(),
                        error: e.to_string(),
                    },
                }
            }
            "vote" => {
                let req: VoteRequest<BrokerNodeId> = serde_json::from_slice(&msg.data)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                
                match self.raft.vote(req).await {
                    Ok(resp) => RaftReply {
                        data: serde_json::to_vec(&resp).unwrap(),
                        error: String::new(),
                    },
                    Err(e) => RaftReply {
                        data: Vec::new(),
                        error: e.to_string(),
                    },
                }
            }
            "install_snapshot" => {
                let req: InstallSnapshotRequest<TypeConfig> = serde_json::from_slice(&msg.data)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                
                match self.raft.install_snapshot(req).await {
                    Ok(resp) => RaftReply {
                        data: serde_json::to_vec(&resp).unwrap(),
                        error: String::new(),
                    },
                    Err(e) => RaftReply {
                        data: Vec::new(),
                        error: e.to_string(),
                    },
                }
            }
            _ => {
                return Err(Status::unimplemented(format!("Unknown RPC type: {}", msg.rpc_type)));
            }
        };
        
        Ok(Response::new(reply))
    }
}

/// Helper to create the gRPC server
pub fn create_raft_grpc_server(raft: Arc<openraft::Raft<TypeConfig>>) -> RaftServer<BrokerRaftService> {
    RaftServer::new(BrokerRaftService::new(raft))
}
