fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting protobuf compilation...");
    
    // Compile Kafka proto to src/api/
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/api/")
        .compile_protos(
            &["src/api/kafka.proto"],
            &["src/api/"]
        )?;
    
    // Compile Raft proto to OUT_DIR (for include_proto!)
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &["src/api/raft.proto"],
            &["src/api/"]
        )?;
    
    println!("Protobuf compilation completed.");
    Ok(())
}