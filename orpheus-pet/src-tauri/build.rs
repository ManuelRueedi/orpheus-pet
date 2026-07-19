fn main() {
    // Tauri watches its config, but an icon-only edit must also rebuild the
    // Windows resource library instead of reusing the previous executable icon.
    println!("cargo:rerun-if-changed=icons/icon.ico");
    tauri_build::build()
}
