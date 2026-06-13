fn main() {
    cc::Build::new()
        .file("src/pw_format.c")
        .include("/usr/include/pipewire-0.3")
        .include("/usr/include/spa-0.2")
        .compile("pw_format");
    println!("cargo:rustc-link-lib=static=pw_format");
    println!("cargo:rustc-link-lib=pipewire-0.3");
}
