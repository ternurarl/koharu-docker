fn main() {
    // Upstream moved the LibTorch shim out of the build: koharu-torch-sys only
    // generates bindings now and the native library arrives as a downloaded
    // package, so there is no DEP_KOHARU_TORCH_SHIM to assert on at build time.
    #[cfg(feature = "gui")]
    tauri_build::build();
}
