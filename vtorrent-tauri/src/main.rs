// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use vtorrent_tauri_lib::{commands, state::AppState};

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            // Wallet lifecycle
            commands::create_wallet,
            commands::open_wallet,
            commands::lock_wallet,
            commands::get_wallet_info,
            // Legacy wallet import
            commands::import_legacy_wallet,
            // Address management
            commands::generate_address,
            commands::get_addresses,
            // 2FA security
            commands::enable_2fa,
            commands::verify_2fa,
            commands::disable_2fa,
            // Node lifecycle
            commands::start_node,
            commands::get_node_info,
            // Transactions
            commands::get_transactions,
            commands::send_vtr,
            // Torrent sessions
            commands::get_torrent_sessions,
            commands::add_torrent,
            commands::remove_torrent,
            // DEX order book
            commands::get_dex_orders,
            commands::place_dex_order,
            commands::cancel_dex_order,
            // Swap lifecycle
            commands::match_dex_order,
            commands::btc_fund,
            commands::vtr_claim,
            commands::btc_claim,
            commands::swap_refund,
            // Staking
            commands::start_staking,
            commands::stop_staking,
            commands::get_staking_status,
            // Bitcoin wallet
            commands::get_btc_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running vTorrent application");
}
