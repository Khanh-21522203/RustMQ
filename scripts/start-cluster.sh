#!/bin/bash

# Script to start a 3-node Raft broker cluster

echo "Building Rust-MQ..."
cargo build --release

# Create data directories
mkdir -p data/broker-1 data/broker-2 data/broker-3

echo "Starting 3-node Raft cluster..."

# Start broker 1 (bootstrap node)
echo "Starting broker 1 (bootstrap)..."
./target/release/Rust-MQ --mode broker --config config/broker-1.yaml > logs/broker-1.log 2>&1 &
BROKER1_PID=$!
echo "Broker 1 started with PID $BROKER1_PID"

# Wait for broker 1 to initialize
sleep 3

# Start broker 2
echo "Starting broker 2..."
./target/release/Rust-MQ --mode broker --config config/broker-2.yaml > logs/broker-2.log 2>&1 &
BROKER2_PID=$!
echo "Broker 2 started with PID $BROKER2_PID"

# Start broker 3
echo "Starting broker 3..."
./target/release/Rust-MQ --mode broker --config config/broker-3.yaml > logs/broker-3.log 2>&1 &
BROKER3_PID=$!
echo "Broker 3 started with PID $BROKER3_PID"

echo ""
echo "Cluster started successfully!"
echo "Broker 1: API=127.0.0.1:9092, RPC=127.0.0.1:19092"
echo "Broker 2: API=127.0.0.1:9093, RPC=127.0.0.1:19093"
echo "Broker 3: API=127.0.0.1:9094, RPC=127.0.0.1:19094"
echo ""
echo "To stop the cluster, run: ./scripts/stop-cluster.sh"
echo ""
echo "Monitor logs with:"
echo "  tail -f logs/broker-1.log"
echo "  tail -f logs/broker-2.log"
echo "  tail -f logs/broker-3.log"

# Save PIDs to file for stop script
echo "$BROKER1_PID" > data/broker-1.pid
echo "$BROKER2_PID" > data/broker-2.pid
echo "$BROKER3_PID" > data/broker-3.pid
