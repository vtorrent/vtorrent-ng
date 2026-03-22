// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Tauri app entry point.
    // In the full Tauri integration, this would call:
    //   tauri::Builder::default()
    //     .manage(AppState::new())
    //     .invoke_handler(tauri::generate_handler![...])
    //     .run(tauri::generate_context!())
    //
    // For now this binary serves as a compilation check.
    // The actual Tauri integration will be wired up when the full
    // Tauri CLI toolchain is installed.
    println!("vTorrent 2.0 backend compiled successfully.");
    println!("Run `cargo tauri dev` to start the full desktop app.");
}
