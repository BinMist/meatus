use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;

    let proto_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let proto_file = proto_dir.join("meatus.proto");

    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc);
    config.compile_protos(&[proto_file], &[proto_dir])?;

    println!("cargo:rerun-if-changed=../../proto/meatus.proto");
    Ok(())
}
