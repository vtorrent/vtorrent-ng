/// HTTP RPC client for vtorrent-cli.
///
/// Makes blocking HTTP requests to the vtorrent-daemon RPC API.

use anyhow::{Context, Result};
use serde_json::Value;

/// A blocking HTTP client for the vTorrent RPC API.
pub struct RpcClient {
    base_url: String,
    client: reqwest::blocking::Client,
}

impl RpcClient {
    /// Create a new RPC client pointing at the given base URL.
    pub fn new(base_url: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        Self { base_url, client }
    }

    /// Make a GET request and return the JSON response.
    pub fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.get(&url)
            .send()
            .with_context(|| format!("Failed to connect to RPC server at {}", self.base_url))?;

        let status = response.status();
        let body: Value = response.json()
            .with_context(|| "Failed to parse JSON response")?;

        if !status.is_success() {
            let err = body["error"].as_str()
                .unwrap_or("unknown error")
                .to_string();
            return Err(anyhow::anyhow!("RPC error ({}): {}", status, err));
        }

        Ok(body)
    }

    /// Make a GET request and return the raw text response.
    pub fn get_text(&self, path: &str) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.get(&url)
            .send()
            .with_context(|| format!("Failed to connect to RPC server at {}", self.base_url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!("RPC error ({})", status));
        }

        let text = response.text()
            .with_context(|| "Failed to read response body")?;
        Ok(text)
    }

    /// Make a POST request with a JSON body and return the JSON response.
    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.post(&url)
            .json(body)
            .send()
            .with_context(|| format!("Failed to connect to RPC server at {}", self.base_url))?;

        let status = response.status();
        let resp_body: Value = response.json()
            .with_context(|| "Failed to parse JSON response")?;

        if !status.is_success() && status.as_u16() != 403 {
            let err = resp_body["error"].as_str()
                .unwrap_or("unknown error")
                .to_string();
            return Err(anyhow::anyhow!("RPC error ({}): {}", status, err));
        }

        Ok(resp_body)
    }

    /// Make a DELETE request and return the JSON response.
    pub fn delete(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.delete(&url)
            .send()
            .with_context(|| format!("Failed to connect to RPC server at {}", self.base_url))?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!("RPC error ({})", status));
        }

        let body: Value = response.json()
            .unwrap_or(serde_json::json!({"success": true}));
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = RpcClient::new("http://127.0.0.1:22525".to_string());
        assert!(client.base_url.contains("22525"));
    }

    #[test]
    fn test_client_connection_refused() {
        // Port 19999 should not be running
        let client = RpcClient::new("http://127.0.0.1:19999".to_string());
        let result = client.get("/api/v1/info");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Failed to connect") || err.contains("Connection refused") || err.contains("error"));
    }
}
