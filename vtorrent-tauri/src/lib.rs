/// vTorrent 2.0 Tauri Backend
///
/// This crate exposes Tauri IPC commands that the React frontend can call
/// via `invoke()`. All sensitive operations (key extraction, encryption,
/// TOTP verification) happen here in Rust — never in JavaScript.
pub mod commands;
pub mod error;
pub mod state;

#[cfg(test)]
mod tests {
    #[test]
    fn default_capability_grants_core_path() {
        let raw = include_str!("../capabilities/default.json");
        let value: serde_json::Value =
            serde_json::from_str(raw).expect("default capability must be valid JSON");
        let permissions = value["permissions"]
            .as_array()
            .expect("default capability must list permissions");
        assert!(permissions.iter().any(|permission| {
            permission
                .as_str()
                .is_some_and(|p| p == "core:default" || p == "core:path:default")
        }));
    }
}
