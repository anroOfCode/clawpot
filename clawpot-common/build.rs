fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                "../proto/clawpot.proto",
                "../proto/clawpot_agent.proto",
                "../proto/network_auth.proto",
            ],
            &["../proto"],
        )?;
    Ok(())
}
