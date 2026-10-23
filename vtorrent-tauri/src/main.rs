// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use vtorrent_tauri_lib::{commands, state::AppState};

fn main() {
    let app = tauri::Builder::default()
        .manage(AppState::new())
        .setup(|app| {
            use tauri::Manager;
            let dir = app.path().app_data_dir()?;
            app.state::<AppState>().set_app_data_dir(dir);
            Ok(())
        })
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
            commands::probe_seed_peers,
            // Transactions
            commands::get_transactions,
            commands::get_activity,
            commands::get_transaction,
            commands::send_vtr,
            commands::quote_vtr,
            commands::create_backup,
            commands::verify_backup,
            commands::restore_backup,
            commands::create_cold_stake,
            commands::fund_cold_stake,
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
            commands::get_swap_status,
            commands::reconcile_btc_swap,
            commands::get_vtr_refund_history,
            commands::bump_vtr_refund,
            // Staking
            commands::start_staking,
            commands::stop_staking,
            commands::get_staking_status,
            commands::get_staking_rewards,
            // Bitcoin wallet
            commands::get_btc_status,
            commands::get_btc_address,
            commands::send_btc,
            // Legacy claims
            commands::check_legacy_claim,
            commands::submit_legacy_claim,
            // Desktop parity: governance, earnings, payment requests, deadlines
            commands::get_governance_params,
            commands::get_governance_proposals,
            commands::get_earnings_summary,
            commands::create_payment_request,
            commands::get_swap_deadlines,
        ])
        .build(tauri::generate_context!())
        .expect("error while building vTorrent application");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            use tauri::Manager;
            let state = app_handle.state::<AppState>();
            tauri::async_runtime::block_on(async {
                let guard = state.node.lock().await;
                if let Some(handle) = guard.as_ref() {
                    if let Some(path) = &handle.mempool_path {
                        let mempool = handle.rpc_state.mempool.lock().await;
                        if let Err(error) = mempool.save_to(path) {
                            tracing::warn!("mempool save on exit failed: {}", error);
                        }
                    }
                }
            });
        }
    });
}
