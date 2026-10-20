use tauri::State;

use crate::{
    error::{Result, TauriError},
    state::AppState,
};

fn rpc_state(
    handle: Option<&crate::state::NodeHandle>,
) -> Result<std::sync::Arc<vtorrent_rpc::state::AppState>> {
    handle
        .map(|handle| std::sync::Arc::new(handle.rpc_state.clone()))
        .ok_or_else(|| TauriError::NodeError("Node not running".into()))
}

#[tauri::command]
pub async fn get_governance_params(
    state: State<'_, AppState>,
) -> Result<vtorrent_rpc::handlers::governance::ParamsResponse> {
    let guard = state.node.lock().await;
    let rpc = rpc_state(guard.as_ref())?;
    let response = vtorrent_rpc::handlers::governance::get_governance_params(axum::extract::State(
        rpc.clone(),
    ))
    .await?;
    Ok(response.0)
}

#[tauri::command]
pub async fn get_governance_proposals(
    state: State<'_, AppState>,
) -> Result<vtorrent_rpc::handlers::governance::ProposalsResponse> {
    let guard = state.node.lock().await;
    let rpc = rpc_state(guard.as_ref())?;
    let response =
        vtorrent_rpc::handlers::governance::get_governance_proposals(axum::extract::State(rpc))
            .await?;
    Ok(response.0)
}

#[tauri::command]
pub async fn get_earnings_summary(
    state: State<'_, AppState>,
    window: String,
) -> Result<vtorrent_rpc::handlers::earnings::EarningsSummary> {
    let guard = state.node.lock().await;
    let rpc = rpc_state(guard.as_ref())?;
    let response = vtorrent_rpc::handlers::earnings::get_earnings_summary(
        axum::extract::State(rpc),
        axum::extract::Query(vtorrent_rpc::handlers::earnings::EarningsQuery {
            window: Some(window),
        }),
    )
    .await?;
    Ok(response.0)
}

#[tauri::command]
pub async fn create_payment_request(
    state: State<'_, AppState>,
    amount_sats: Option<u64>,
    label: Option<String>,
    message: Option<String>,
) -> Result<vtorrent_rpc::handlers::wallet::PaymentRequestResp> {
    let guard = state.node.lock().await;
    let rpc = rpc_state(guard.as_ref())?;
    let request = vtorrent_rpc::handlers::wallet::PaymentRequestReq {
        address: None,
        amount_sats,
        label,
        message,
        req: None,
    };
    let response = vtorrent_rpc::handlers::wallet::create_payment_request(
        axum::extract::State(rpc),
        axum::Json(request),
    )
    .await?;
    Ok(response.0)
}

#[tauri::command]
pub async fn get_swap_deadlines(
    state: State<'_, AppState>,
) -> Result<Vec<vtorrent_rpc::swap_reconciliation::SwapDeadline>> {
    let guard = state.node.lock().await;
    let rpc = rpc_state(guard.as_ref())?;
    let response =
        vtorrent_rpc::swap_reconciliation::get_swap_deadlines(axum::extract::State(rpc)).await?;
    Ok(response.0.deadlines)
}
