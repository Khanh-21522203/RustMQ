/// Simple performance benchmark for Rust-MQ
/// 
/// Tests:
/// - Producer throughput with different message sizes
/// - Consumer throughput
/// - End-to-end latency
/// - Batch performance

use anyhow::Result;
use rust_mq::broker::{kafka_broker_server::KafkaBrokerServer, core::BrokerCore, storage::InMemoryStorage};
use rust_mq::client::{
    Producer, Consumer, ProducerConfig, ConsumerConfig,
    ProducerMessage, ConsumedMessage, MessageHandler,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

struct BenchmarkHandler {
    counter: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl MessageHandler for BenchmarkHandler {
    async fn handle(&mut self, _message: ConsumedMessage) -> Result<()> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn start_broker(addr: &str) -> Result<()> {
    let (rpc_tx, rpc_rx) = mpsc::channel(10000);
    let storage = InMemoryStorage::new(1, "localhost".to_string(), 50051);
    let mut broker_core = BrokerCore::new(rpc_rx, storage);
    
    tokio::spawn(async move {
        broker_core.run().await;
    });
    
    let grpc_server = KafkaBrokerServer::new(rpc_tx);
    let addr_owned = addr.to_string();
    
    tokio::spawn(async move {
        if let Err(e) = grpc_server.run(&addr_owned).await {
            eprintln!("Broker error: {}", e);
        }
    });
    
    sleep(Duration::from_millis(500)).await;
    Ok(())
}

async fn benchmark_producer_throughput(message_count: usize, message_size: usize, batch_size: usize) -> Result<(Duration, f64)> {
    let config = ProducerConfig {
        topic: "bench-topic".to_string(),
        partition: 0,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size,
        flush_interval_ms: 100,
    };
    
    let mut producer = Producer::new("http://localhost:50051", config).await?;
    producer.start().await?;
    
    let message = vec![0u8; message_size];
    let start = Instant::now();
    
    for _ in 0..message_count {
        let msg = ProducerMessage::new(message.clone());
        producer.send(msg).await?;
    }
    
    producer.flush().await?;
    let duration = start.elapsed();
    
    producer.shutdown().await?;
    
    let throughput = message_count as f64 / duration.as_secs_f64();
    Ok((duration, throughput))
}

async fn benchmark_e2e_latency(message_count: usize) -> Result<(Duration, Vec<Duration>)> {
    let producer_config = ProducerConfig {
        topic: "e2e-topic".to_string(),
        partition: 0,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 1,
        flush_interval_ms: 10,
    };
    
    let consumer_config = ConsumerConfig {
        topic: "e2e-topic".to_string(),
        partition: 0,
        group_id: Some("bench-group".to_string()),
        offset: -2,
        max_bytes: 1_048_576,
        max_wait_ms: 100,
        min_bytes: 1,
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 10,
    };
    
    let mut producer = Producer::new("http://localhost:50051", producer_config).await?;
    producer.start().await?;
    
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    
    let mut consumer = Consumer::new("http://localhost:50051", consumer_config).await?;
    let handler = BenchmarkHandler { counter: counter_clone };
    consumer.start(handler).await?;
    
    let mut latencies = Vec::new();
    let total_start = Instant::now();
    
    for _ in 0..message_count {
        let start = Instant::now();
        let msg = ProducerMessage::new(b"benchmark message".to_vec());
        producer.send(msg).await?;
        producer.flush().await?;
        
        // Wait for message to be received
        let initial_count = counter.load(Ordering::SeqCst);
        let mut attempts = 0;
        while counter.load(Ordering::SeqCst) == initial_count && attempts < 100 {
            sleep(Duration::from_millis(1)).await;
            attempts += 1;
        }
        
        latencies.push(start.elapsed());
    }
    
    let total_duration = total_start.elapsed();
    
    consumer.shutdown().await?;
    producer.shutdown().await?;
    
    Ok((total_duration, latencies))
}

async fn benchmark_consumer_throughput(message_count: usize) -> Result<(Duration, f64)> {
    // First, populate messages
    let producer_config = ProducerConfig {
        topic: "consumer-bench-topic".to_string(),
        partition: 0,
        required_acks: 1,
        timeout_ms: 5000,
        batch_size: 100,
        flush_interval_ms: 100,
    };
    
    let mut producer = Producer::new("http://localhost:50051", producer_config).await?;
    producer.start().await?;
    
    for i in 0..message_count {
        let msg = ProducerMessage::new(format!("Message {}", i).as_bytes().to_vec());
        producer.send(msg).await?;
    }
    producer.flush().await?;
    producer.shutdown().await?;
    
    // Now benchmark consumer
    let consumer_config = ConsumerConfig {
        topic: "consumer-bench-topic".to_string(),
        partition: 0,
        group_id: Some("consumer-bench-group".to_string()),
        offset: -2,
        max_bytes: 1_048_576,
        max_wait_ms: 100,
        min_bytes: 1,
        auto_commit: true,
        auto_commit_interval_ms: 5000,
        poll_interval_ms: 10,
    };
    
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    
    let mut consumer = Consumer::new("http://localhost:50051", consumer_config).await?;
    let handler = BenchmarkHandler { counter: counter_clone };
    consumer.start(handler).await?;
    
    let start = Instant::now();
    
    // Wait for all messages to be consumed
    while counter.load(Ordering::SeqCst) < message_count {
        sleep(Duration::from_millis(10)).await;
    }
    
    let duration = start.elapsed();
    consumer.shutdown().await?;
    
    let throughput = message_count as f64 / duration.as_secs_f64();
    Ok((duration, throughput))
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("╔════════════════════════════════════════════════════════╗");
    println!("║         Rust-MQ Performance Benchmark                  ║");
    println!("╚════════════════════════════════════════════════════════╝\n");
    
    // Start broker
    println!("🚀 Starting broker...");
    start_broker("0.0.0.0:50051").await?;
    sleep(Duration::from_secs(1)).await;
    println!("✓ Broker started\n");
    
    // Test 1: Producer throughput with different message sizes
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📊 Test 1: Producer Throughput");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    let test_cases = vec![
        (10000, 100, 100, "Small messages (100B)"),
        (5000, 1000, 100, "Medium messages (1KB)"),
        (1000, 10000, 100, "Large messages (10KB)"),
    ];
    
    for (count, size, batch, desc) in test_cases {
        print!("Testing {}... ", desc);
        let (duration, throughput) = benchmark_producer_throughput(count, size, batch).await?;
        let mb_per_sec = (throughput * size as f64) / 1_048_576.0;
        println!("✓");
        println!("  Duration: {:.2}s", duration.as_secs_f64());
        println!("  Throughput: {:.0} msg/s ({:.2} MB/s)", throughput, mb_per_sec);
        println!();
        sleep(Duration::from_millis(500)).await;
    }
    
    // Test 2: Batch performance
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📦 Test 2: Batch Size Impact");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    let batch_sizes = vec![
        (10, "Batch size 10"),
        (100, "Batch size 100"),
        (1000, "Batch size 1000"),
    ];
    
    for (batch_size, desc) in batch_sizes {
        print!("Testing {}... ", desc);
        let (duration, throughput) = benchmark_producer_throughput(5000, 100, batch_size).await?;
        println!("✓");
        println!("  Duration: {:.2}s", duration.as_secs_f64());
        println!("  Throughput: {:.0} msg/s", throughput);
        println!();
        sleep(Duration::from_millis(500)).await;
    }
    
    // Test 3: End-to-end latency
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("⚡ Test 3: End-to-End Latency");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    print!("Measuring latency for 100 messages... ");
    let (total_duration, latencies) = benchmark_e2e_latency(100).await?;
    println!("✓");
    
    let avg_latency = latencies.iter().map(|d| d.as_micros()).sum::<u128>() / latencies.len() as u128;
    let min_latency = latencies.iter().map(|d| d.as_micros()).min().unwrap();
    let max_latency = latencies.iter().map(|d| d.as_micros()).max().unwrap();
    
    // Calculate p50, p95, p99
    let mut sorted_latencies: Vec<_> = latencies.iter().map(|d| d.as_micros()).collect();
    sorted_latencies.sort();
    let p50 = sorted_latencies[sorted_latencies.len() * 50 / 100];
    let p95 = sorted_latencies[sorted_latencies.len() * 95 / 100];
    let p99 = sorted_latencies[sorted_latencies.len() * 99 / 100];
    
    println!("  Total time: {:.2}s", total_duration.as_secs_f64());
    println!("  Average latency: {:.2}ms", avg_latency as f64 / 1000.0);
    println!("  Min latency: {:.2}ms", min_latency as f64 / 1000.0);
    println!("  Max latency: {:.2}ms", max_latency as f64 / 1000.0);
    println!("  p50 latency: {:.2}ms", p50 as f64 / 1000.0);
    println!("  p95 latency: {:.2}ms", p95 as f64 / 1000.0);
    println!("  p99 latency: {:.2}ms", p99 as f64 / 1000.0);
    println!();
    
    sleep(Duration::from_millis(500)).await;
    
    // Test 4: Consumer throughput
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📥 Test 4: Consumer Throughput");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    print!("Testing consumer with 10,000 messages... ");
    let (duration, throughput) = benchmark_consumer_throughput(10000).await?;
    println!("✓");
    println!("  Duration: {:.2}s", duration.as_secs_f64());
    println!("  Throughput: {:.0} msg/s", throughput);
    println!();
    
    // Summary
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("✅ Benchmark Complete!");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    Ok(())
}
