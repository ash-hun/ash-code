//! Compile `proto/ash.proto` into Rust code at build time via tonic-build.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proto_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or("cannot resolve workspace root")?
        .join("proto");
    let proto_file = proto_root.join("ash.proto");

    println!("cargo:rerun-if-changed={}", proto_file.display());

    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&[proto_file], &[proto_root])?;

    Ok(())
}
