fn main() {
    // No app commands: the shell exposes none (the barcode plugin registers
    // its own), so no app manifest is needed.
    tauri_build::build();
}
