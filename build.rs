fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting protobuf compilation...");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/api/")
        .compile_protos(
            &["src/api/kafka.proto"],
            &["src/api/"]
        )?;
    println!("Protobuf compilation completed.");
    Ok(())
}