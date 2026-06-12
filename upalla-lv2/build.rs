fn main() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let model_dir = format!("{}/.local/share/upalla", home);
    let enc = format!("{}/enc.onnx", model_dir);

    let model_url = "https://huggingface.co/Serkan007/DeepFilterNet3-ONNX/resolve/main/DeepFilterNet3_onnx.tar.gz";

    if !std::path::Path::new(&enc).exists() {
        println!("cargo:warning=╔══════════════════════════════════════════════════════════╗");
        println!("cargo:warning=║  Upalla: DeepFilterNet3 ONNX model not found            ║");
        println!("cargo:warning=║                                                        ║");
        println!("cargo:warning=║  Expected directory: {:<35}║", model_dir);
        println!("cargo:warning=║  With files: enc.onnx, erb_dec.onnx, df_dec.onnx       ║");
        println!("cargo:warning=║                                                        ║");
        println!("cargo:warning=║  Download: ./scripts/download-model.sh                 ║");
        println!("cargo:warning=╚══════════════════════════════════════════════════════════╝");
    } else {
        println!(
            "cargo:warning=Upalla: model found at {}/",
            model_dir
        );
    }

    println!("cargo:rustc-env=UPALLA_MODEL_URL={}", model_url);
}
