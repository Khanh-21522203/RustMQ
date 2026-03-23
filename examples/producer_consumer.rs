/// Complete example demonstrating producer-consumer communication
/// 
/// This example:
/// 1. Starts a broker in the background
/// 2. Creates a producer that sends messages
/// 3. Creates a consumer that receives and processes messages
/// 4. Demonstrates graceful shutdown

use anyhow::Result;
use rust_mq::broker::{core::BrokerCore, kafka_broker_server::KafkaBrokerServer, storage::InMemoryStorage};
use rust_mq::client::{
    ConsumedMessage, Consumer, ConsumerConfig, MessageHandler, Producer, ProducerConfig,
    ProducerMessage,
};
use rust_mq::codec::{decode, encode, MessageEnvelope};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DemoPayload {
    sequence: u32,
    content: String,
    source: String,
}

/// Custom message handler that counts processed messages
struct CountingHandler {
    counter: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl MessageHandler for CountingHandler {
    async fn handle(&self, message: ConsumedMessage) -> Result<()> {
        let count = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let envelope: MessageEnvelope<DemoPayload> = decode(&message.value)?;

        println!(
            "[Consumer] Message #{}: [{}:{}:{}] event={} v{} seq={} source={} content={}",
            count,
            message.topic,
            message.partition,
            message.offset,
            envelope.event_type,
            envelope.schema_version,
            envelope.payload.sequence,
            envelope.payload.source,
            envelope.payload.content
        );

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info")
    ).init();
    
    println!("=== Rust-MQ Producer-Consumer Example ===\n");
    
    // 1. Start broker
    println!("Step 1: Starting broker...");
    let broker_addr = "0.0.0.0:50051";
    start_broker(broker_addr).await?;
    
    // Give broker time to start
    sleep(Duration::from_secs(1)).await;
    println!("✓ Broker started at {}\n", broker_addr);
    
    // 2. Create and start producer
    println!("Step 2: Creating producer...");
    let producer_config = ProducerConfig {
        topic: "demo-topic".to_string(),
        partition: 0,
        partitioning: "fixed".to_string(),
        num_partitions: 1,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 10,
        flush_interval_ms: 100,
    };
    
    let producer = Producer::new(
        &format!("http://{}", broker_addr),
        producer_config,
    ).await?;
    
    println!("✓ Producer started\n");
    
    // 3. Create and start consumer
    println!("Step 3: Creating consumer...");
    let consumer_config = ConsumerConfig {
        topic: "demo-topic".to_string(),
        partitions: vec![0],
        group_id: Some("demo-group".to_string()),
        offset: -2, // Start from earliest
        max_bytes: 1_048_576,
        max_wait_ms: 1000,
        min_bytes: 1,
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 500,
    };
    
    let message_counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = message_counter.clone();
    
    let mut consumer = Consumer::new(
        &format!("http://{}", broker_addr),
        consumer_config,
    ).await?;
    
    let handler = CountingHandler {
        counter: counter_clone,
    };
    
    consumer.start(handler).await?;
    println!("✓ Consumer started\n");
    
    // 4. Send messages
    println!("Step 4: Sending messages...");
    println!("----------------------------------");

    for i in 1..=20 {
        let payload = DemoPayload {
            sequence: i,
            content: format!("Message number {}", i),
            source: "batch-producer".to_string(),
        };
        let envelope = MessageEnvelope::new("demo.message", 1, payload.clone());
        let msg = ProducerMessage::new(encode(&envelope)?);

        producer.send(msg).await?;
        println!("[Producer] Sent: {}", payload.content);

        // Small delay to make output readable
        sleep(Duration::from_millis(100)).await;
    }
    
    // Flush any pending messages
    producer.flush().await?;
    println!("----------------------------------");
    println!("✓ All messages sent\n");
    
    // 5. Wait for consumer to process all messages
    println!("Step 5: Waiting for consumer to process messages...");
    for _ in 0..10 {
        let count = message_counter.load(Ordering::SeqCst);
        if count >= 20 {
            break;
        }
        sleep(Duration::from_millis(500)).await;
    }
    
    let final_count = message_counter.load(Ordering::SeqCst);
    println!("✓ Consumer processed {} messages\n", final_count);
    
    // 6. Demonstrate synchronous send
    println!("Step 6: Demonstrating synchronous send...");
    let sync_payload = DemoPayload {
        sequence: 21,
        content: "Synchronous message".to_string(),
        source: "sync-producer".to_string(),
    };
    let sync_envelope = MessageEnvelope::new("demo.sync", 1, sync_payload);
    let result = producer.send_sync(
        ProducerMessage::new(encode(&sync_envelope)?)
    ).await?;
    
    println!(
        "✓ Synchronous message sent to partition {} at offset {}\n",
        result.partition,
        result.offset
    );
    
    // Wait for consumer to process the sync message
    sleep(Duration::from_secs(1)).await;
    
    // 7. Graceful shutdown
    println!("Step 7: Shutting down...");
    
    consumer.shutdown().await?;
    println!("✓ Consumer stopped");
    
    producer.shutdown().await?;
    println!("✓ Producer stopped");
    
    println!("\n=== Example Complete ===");
    println!("\nSummary:");
    println!("  - Messages sent: 21");
    println!("  - Messages received: {}", message_counter.load(Ordering::SeqCst));
    println!("  - Topic: demo-topic");
    println!("  - Consumer Group: demo-group");
    
    Ok(())
}

async fn start_broker(addr: &str) -> Result<()> {
    // Create channel for RPC communication
    let (rpc_tx, rpc_rx) = mpsc::channel(1000);
    
    // Create storage
    let storage = InMemoryStorage::new(1, "localhost".to_string(), 50051);
    
    // Create broker core
    let broker_core = BrokerCore::new(rpc_rx, storage);

    // Start broker core in background
    tokio::spawn(async move {
        broker_core.run().await;
    });
    
    // Create and start gRPC server
    let grpc_server = KafkaBrokerServer::new(rpc_tx);
    
    // Convert addr to String for 'static lifetime
    let addr_owned = addr.to_string();
    
    // Start server in background
    tokio::spawn(async move {
        if let Err(e) = grpc_server.run(&addr_owned).await {
            log::error!("Failed to start broker: {}", e);
        }
    });
    
    Ok(())
}
