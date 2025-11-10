/// Comprehensive benchmarks for Rust-MQ
/// 
/// Measures:
/// - Producer throughput
/// - Consumer throughput  
/// - End-to-end latency
/// - Different message sizes
/// - Different batch sizes

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rust_mq::broker::{kafka_broker_server::KafkaBrokerServer, core::BrokerCore, storage::InMemoryStorage};
use rust_mq::client::{
    Producer, Consumer, ProducerConfig, ConsumerConfig,
    ProducerMessage, ConsumedMessage, MessageHandler,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

/// Simple message handler for benchmarks
struct BenchmarkHandler {
    counter: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl MessageHandler for BenchmarkHandler {
    async fn handle(&mut self, _message: ConsumedMessage) -> anyhow::Result<()> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Start a broker instance for benchmarking
async fn start_benchmark_broker(addr: &str) -> anyhow::Result<()> {
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

/// Benchmark producer throughput
fn bench_producer_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("producer_throughput");
    
    // Test different message sizes
    for msg_size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Bytes(*msg_size as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}B", msg_size)),
            msg_size,
            |b, &size| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async move {
                        let config = ProducerConfig {
                            topic: "bench-topic".to_string(),
                            partition: 0,
                            required_acks: 1,
                            timeout_ms: 5000,
                            batch_size: 100,
                            flush_interval_ms: 100,
                        };
                        
                        let mut producer = Producer::new(
                            "http://localhost:50051",
                            config,
                        ).await.unwrap();
                        
                        producer.start().await.unwrap();
                        
                        let message = vec![0u8; size];
                        let msg = ProducerMessage::new(black_box(message));
                        
                        producer.send(msg).await.unwrap();
                        producer.flush().await.unwrap();
                        producer.shutdown().await.unwrap();
                    });
            },
        );
    }
    
    group.finish();
}

/// Benchmark batch producer throughput
fn bench_batch_producer_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_producer_throughput");
    
    // Test different batch sizes
    for batch_size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}_messages", batch_size)),
            batch_size,
            |b, &size| {
                b.to_async(tokio::runtime::Runtime::new().unwrap())
                    .iter(|| async move {
                        let config = ProducerConfig {
                            topic: "bench-batch-topic".to_string(),
                            partition: 0,
                            required_acks: 1,
                            timeout_ms: 5000,
                            batch_size: size,
                            flush_interval_ms: 1000,
                        };
                        
                        let mut producer = Producer::new(
                            "http://localhost:50051",
                            config,
                        ).await.unwrap();
                        
                        producer.start().await.unwrap();
                        
                        for _ in 0..size {
                            let message = vec![0u8; 100];
                            let msg = ProducerMessage::new(black_box(message));
                            producer.send(msg).await.unwrap();
                        }
                        
                        producer.flush().await.unwrap();
                        producer.shutdown().await.unwrap();
                    });
            },
        );
    }
    
    group.finish();
}

/// Benchmark end-to-end latency
fn bench_e2e_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end_latency");
    group.sample_size(50);
    
    group.bench_function("single_message", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                // Producer config
                let producer_config = ProducerConfig {
                    topic: "e2e-topic".to_string(),
                    partition: 0,
                    required_acks: 1,
                    timeout_ms: 5000,
                    batch_size: 1,
                    flush_interval_ms: 10,
                };
                
                // Consumer config
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
                
                // Create producer
                let mut producer = Producer::new(
                    "http://localhost:50051",
                    producer_config,
                ).await.unwrap();
                producer.start().await.unwrap();
                
                // Create consumer
                let counter = Arc::new(AtomicUsize::new(0));
                let counter_clone = counter.clone();
                
                let mut consumer = Consumer::new(
                    "http://localhost:50051",
                    consumer_config,
                ).await.unwrap();
                
                let handler = BenchmarkHandler { counter: counter_clone };
                consumer.start(handler).await.unwrap();
                
                // Send and receive message
                let msg = ProducerMessage::new(black_box(b"benchmark message".to_vec()));
                producer.send(msg).await.unwrap();
                producer.flush().await.unwrap();
                
                // Wait for message to be received
                let mut attempts = 0;
                while counter.load(Ordering::SeqCst) == 0 && attempts < 100 {
                    sleep(Duration::from_millis(10)).await;
                    attempts += 1;
                }
                
                // Cleanup
                consumer.shutdown().await.unwrap();
                producer.shutdown().await.unwrap();
            });
    });
    
    group.finish();
}

/// Benchmark high throughput scenario
fn bench_high_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_throughput");
    group.sample_size(20);
    
    let message_count = 10000;
    group.throughput(Throughput::Elements(message_count));
    
    group.bench_function("10k_messages", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let config = ProducerConfig {
                    topic: "high-throughput-topic".to_string(),
                    partition: 0,
                    required_acks: 0, // Fire and forget for max throughput
                    timeout_ms: 5000,
                    batch_size: 1000,
                    flush_interval_ms: 100,
                };
                
                let mut producer = Producer::new(
                    "http://localhost:50051",
                    config,
                ).await.unwrap();
                
                producer.start().await.unwrap();
                
                for i in 0..message_count {
                    let message = format!("Message {}", i);
                    let msg = ProducerMessage::new(black_box(message.as_bytes().to_vec()));
                    producer.send(msg).await.unwrap();
                }
                
                producer.flush().await.unwrap();
                producer.shutdown().await.unwrap();
            });
    });
    
    group.finish();
}

/// Setup function to start broker before benchmarks
fn setup_broker() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _ = start_benchmark_broker("0.0.0.0:50051").await;
        sleep(Duration::from_secs(1)).await;
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(std::time::Duration::from_secs(3))
        .measurement_time(std::time::Duration::from_secs(10));
    targets = 
        bench_producer_throughput,
        bench_batch_producer_throughput,
        bench_e2e_latency,
        bench_high_throughput
}

criterion_main!(benches);
