use std::time::Duration;

use anyhow::Result;
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("LRCLIB HTTP {status_code} ({name}): {message}")]
pub struct ApiError {
    pub status_code: u16,
    name: String,
    message: String,
}

pub fn api_error(status: StatusCode, body: &Value) -> anyhow::Error {
    let name = body
        .get("error")
        .or_else(|| body.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("HTTP error");
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| status.canonical_reason())
        .unwrap_or("Unknown error");
    ApiError {
        status_code: status.as_u16(),
        name: name.to_owned(),
        message: message.to_owned(),
    }
    .into()
}

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

#[cfg(test)]
mod tests {
    use super::api_error;
    use reqwest::StatusCode;
    use serde_json::json;

    #[test]
    fn accepts_lrclib_validation_error_shape() {
        let error = api_error(
            StatusCode::BAD_REQUEST,
            &json!({"name": "ValidationError", "statusCode": 400, "message": "invalid track_name"}),
        );
        assert_eq!(error.to_string(), "LRCLIB HTTP 400 (ValidationError): invalid track_name");
    }
}
