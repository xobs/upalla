fn main() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let model_path = std::path::PathBuf::from(format!(
        "{}/.local/share/upalla/deepfilter.onnx",
        home
    ));

    let model_url = "https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz";

    if !model_path.exists() {
        println!("cargo:warning=╔══════════════════════════════════════════════════════════╗");
        println!("cargo:warning=║  Upalla: DeepFilterNet3 ONNX model not found            ║");
        println!("cargo:warning=║                                                        ║");
        println!(
            "cargo:warning=║  Expected: {:<43}║",
            model_path.display()
        );
        println!("cargo:warning=║                                                        ║");
        println!("cargo:warning=║  Download: ./scripts/download-model.sh                 ║");
        println!("cargo:warning=║  Or: wget {} ║", model_url);
        println!("cargo:warning=╚══════════════════════════════════════════════════════════╝");
    } else {
        println!(
            "cargo:warning=Upalla: model found at {}",
            model_path.display()
        );
    }

    println!("cargo:rustc-env=UPALLA_MODEL_URL={}", model_url);
}
