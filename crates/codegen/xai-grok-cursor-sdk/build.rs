fn main() {
    let proto_root = std::path::Path::new("../../../third_party/cursor-sdk-bridge/proto");
    let files = [
        proto_root.join("sdk/v1/sdk_agent_service.proto"),
        proto_root.join("sdk/v1/sdk_bridge_control_service.proto"),
        proto_root.join("sdk/v1/sdk_cursor_service.proto"),
        proto_root.join("sdk/v1/sdk_errors.proto"),
        proto_root.join("sdk/v1/sdk_messages.proto"),
    ];
    // prost-build rejects absolute proto paths; resolve from this crate.
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("chdir crate root");
    xai_proto_build::configure()
        .compile_protos(&files, &[proto_root])
        .expect("compile cursor sdk.v1 protos");
}
