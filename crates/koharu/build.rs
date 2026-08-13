fn main() {
    std::env::var_os("DEP_KOHARU_TORCH_SHIM")
        .expect("koharu-torch-sys did not provide its native shim");
    #[cfg(feature = "gui")]
    tauri_build::build();
}
