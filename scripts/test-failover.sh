#!/bin/bash

# Script to test failover in Raft cluster

echo "Testing failover in Raft cluster..."

# Start a producer connected to broker 1
echo "Starting producer connected to broker 1 (127.0.0.1:9092)..."
./target/release/Rust-MQ --mode producer --broker 127.0.0.1:9092 &
PRODUCER_PID=$!

# Start a consumer connected to broker 2
echo "Starting consumer connected to broker 2 (127.0.0.1:9093)..."
./target/release/Rust-MQ --mode consumer --broker 127.0.0.1:9093 &
CONSUMER_PID=$!

echo ""
echo "Producer and consumer started."
echo "Try the following tests:"
echo "1. Send messages through the producer"
echo "2. Stop the leader broker (check logs to identify leader)"
echo "3. Observe that messages continue to flow after leader election"
echo ""
echo "To identify the leader, check logs:"
echo "  grep 'leader' logs/broker-*.log"
echo ""
echo "To stop a specific broker:"
echo "  kill \$(cat data/broker-N.pid)  # where N is 1, 2, or 3"
echo ""
echo "Press Ctrl+C to stop this test"

# Wait for interrupt
trap "kill $PRODUCER_PID $CONSUMER_PID 2>/dev/null; exit" INT
wait
