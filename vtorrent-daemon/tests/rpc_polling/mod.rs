use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

pub(super) async fn get_json(client: &Client, url: &str) -> Result<Value, reqwest::Error> {
    client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
}

pub(super) async fn wait_tip(
    client: &Client,
    url: &str,
    expected: &str,
    timeout: Duration,
) -> Result<(), String> {
    let mut last_observation = "no response received".to_owned();
    tokio::time::timeout(timeout, async {
        loop {
            match get_json(client, url).await {
                Ok(info) => {
                    let tip = info["best_block_hash"]
                        .as_str()
                        .ok_or_else(|| format!("missing or invalid best_block_hash: {info}"))?;
                    if tip == expected {
                        return Ok(());
                    }
                    last_observation = format!("observed tip {tip}");
                }
                Err(error) if error.is_timeout() || error.is_connect() => {
                    last_observation = format!("RPC transport error: {error}");
                }
                Err(error) => return Err(format!("RPC response failed: {error}")),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        Err(format!(
            "timed out waiting for tip {expected} at {url}; {last_observation}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct Server {
        url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl Server {
        async fn start(app: Router) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/info", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self { url, task }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn client(timeout: Duration) -> Client {
        Client::builder()
            .no_proxy()
            .timeout(timeout)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn retries_timeout_then_stale_tip_until_expected_tip() {
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let server = Server::start(Router::new().route(
            "/info",
            get(move || {
                let request = count.fetch_add(1, Ordering::SeqCst);
                async move {
                    match request {
                        0 => std::future::pending::<&str>().await,
                        1 => r#"{"best_block_hash":"old"}"#,
                        _ => r#"{"best_block_hash":"expected"}"#,
                    }
                }
            }),
        ))
        .await;
        wait_tip(
            &client(Duration::from_millis(100)),
            &server.url,
            "expected",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(requests.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn deadline_bounds_a_stalled_request() {
        let server =
            Server::start(Router::new().route("/info", get(std::future::pending::<&str>))).await;
        let error = wait_tip(
            &client(Duration::from_secs(10)),
            &server.url,
            "expected",
            Duration::from_millis(100),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out waiting for tip expected"));
        assert!(error.contains("no response received"));
    }

    #[tokio::test]
    async fn deadline_reports_last_observed_tip() {
        let server = Server::start(
            Router::new().route("/info", get(|| async { r#"{"best_block_hash":"old"}"# })),
        )
        .await;
        let error = wait_tip(
            &client(Duration::from_secs(1)),
            &server.url,
            "expected",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out waiting for tip expected"));
        assert!(error.contains("observed tip old"));
    }

    #[tokio::test]
    async fn deadline_reports_repeated_transport_timeouts() {
        let server =
            Server::start(Router::new().route("/info", get(std::future::pending::<&str>))).await;
        let error = wait_tip(
            &client(Duration::from_millis(50)),
            &server.url,
            "expected",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.contains("timed out waiting for tip expected"));
        assert!(error.contains("RPC transport error"));
    }

    #[tokio::test]
    async fn rejects_invalid_responses_without_retrying() {
        for (status, body) in [
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "{}"),
            (axum::http::StatusCode::OK, "not JSON"),
            (axum::http::StatusCode::OK, "{}"),
            (axum::http::StatusCode::OK, r#"{"best_block_hash":123}"#),
        ] {
            let requests = Arc::new(AtomicUsize::new(0));
            let count = requests.clone();
            let server = Server::start(Router::new().route(
                "/info",
                get(move || {
                    count.fetch_add(1, Ordering::SeqCst);
                    async move { (status, body) }
                }),
            ))
            .await;
            let error = wait_tip(
                &client(Duration::from_secs(1)),
                &server.url,
                "expected",
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
            assert!(!error.contains("timed out"), "{error}");
            assert_eq!(requests.load(Ordering::SeqCst), 1);
        }
    }
}
