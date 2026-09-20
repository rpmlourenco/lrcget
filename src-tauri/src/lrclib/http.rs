use std::time::Duration;

use anyhow::Result;
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;

/// Retry only read-only requests and transient transport/server failures.
pub async fn get_json(client: &Client, url: Url) -> Result<(StatusCode, Value)> {
    for attempt in 0..3 {
        let response = client.get(url.clone()).send().await;
        match response {
            Ok(response) => {
                let status = response.status();
                if attempt < 2
                    && (status == StatusCode::TOO_MANY_REQUESTS
                        || status == StatusCode::BAD_GATEWAY
                        || status == StatusCode::SERVICE_UNAVAILABLE
                        || status == StatusCode::GATEWAY_TIMEOUT)
                {
                    tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
                    continue;
                }
                if status == StatusCode::NOT_FOUND {
                    return Ok((status, Value::Null));
                }
                match response.json::<Value>().await {
                    Ok(body) => return Ok((status, body)),
                    Err(error) if attempt < 2 && (error.is_body() || error.is_decode() || error.is_timeout()) => {
                        tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if attempt < 2 && (error.is_connect() || error.is_timeout() || error.is_body()) => {
                tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!()
}
