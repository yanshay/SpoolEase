// use std::env;
// use std::path::Path;
// use std::process::Command;

fn main() {
    // // Run wasm-pack build in the second project directory
    //
    // let device_wasm_project_path = std::path::Path::new("../device-wasm");
    //
    // let mut cmd = std::process::Command::new("wasm-pack");
    // let cmd = cmd
    //     .arg("build")
    //     .arg("--release")
    //     .arg("--target")
    //     .arg("web") // or another target like "nodejs"
    //     .current_dir(device_wasm_project_path);
    //
    // for (key, _) in std::env::vars() {
    //     if key.starts_with("CARGO") {
    //         cmd.env_remove(&key);
    //     }
    // }
    //
    // cmd.env_remove("RUSTFLAGS");
    // cmd.env_remove("RUSTUP_TOOLCHAIN"); // Ensures rustup picks the correct one
    // cmd.env_remove("RUSTC");
    //
    // let status = cmd.status().expect("Failed to execute wasm-pack");
    //
    // if !status.success() {
    //     panic!("wasm-pack build failed");
    // }
    //
    // // Now, the second project is built, and you can include the output
    // let output_dir = device_wasm_project_path.join("pkg");
    // let output_file = output_dir.join("device_wasm_bg.wasm");
    //
    // // Make sure the file exists
    // if !output_file.exists() {
    //     panic!("Output file not found after wasm-pack build");
    // }
    //
    // // println!("cargo:rerun-if-changed={}", output_file.display());
    // println!("cargo:rerun-if-changed={}", device_wasm_project_path.display());


    // Slint needs to come last, seems like it syncs in some way with the build and waits to the end
    slint_build::compile_with_config(
        "ui/appwindow.slint",
        slint_build::CompilerConfiguration::new().embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}

