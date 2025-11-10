#!/bin/bash

# Script to stop the Raft broker cluster

echo "Stopping Raft cluster..."

# Stop each broker
if [ -f data/broker-1.pid ]; then
    PID=$(cat data/broker-1.pid)
    echo "Stopping broker 1 (PID: $PID)..."
    kill $PID 2>/dev/null
    rm data/broker-1.pid
fi

if [ -f data/broker-2.pid ]; then
    PID=$(cat data/broker-2.pid)
    echo "Stopping broker 2 (PID: $PID)..."
    kill $PID 2>/dev/null
    rm data/broker-2.pid
fi

if [ -f data/broker-3.pid ]; then
    PID=$(cat data/broker-3.pid)
    echo "Stopping broker 3 (PID: $PID)..."
    kill $PID 2>/dev/null
    rm data/broker-3.pid
fi

echo "Cluster stopped."
