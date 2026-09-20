use std::time::Duration;

use anyhow::Result;
use reqwest;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchItem {
    pub(crate) id: i64,
    pub(crate) name: Option<String>,
    pub(crate) artist_name: Option<String>,
    pub(crate) album_name: Option<String>,
    pub(crate) duration: Option<f64>,
    pub(crate) instrumental: bool,
    pub(crate) plain_lyrics: Option<String>,
    pub(crate) synced_lyrics: Option<String>,
    pub(crate) lyricsfile: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct Response(Vec<SearchItem>);

impl Response {
    pub(crate) fn into_items(self) -> Vec<SearchItem> {
        self.0
    }
}

#[derive(Error, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[error("{error}: {message}")]
pub struct ResponseError {
    status_code: Option<u16>,
    error: String,
    message: String,
}

pub async fn request(
    title: &str,
    album_name: &str,
    artist_name: &str,
    q: &str,
    lrclib_instance: &str,
) -> Result<Response> {
    let params: Vec<(&str, &str)> = [
        ("track_name", title),
        ("artist_name", artist_name),
        ("album_name", album_name),
        ("q", q),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .collect();

    let version = env!("CARGO_PKG_VERSION");
    let user_agent = format!(
        "LRCGET v{} (https://github.com/tranxuanthang/lrcget)",
        version
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(user_agent)
        .build()?;
    let api_endpoint = format!("{}/api/search", lrclib_instance.trim_end_matches('/'));
    let url = reqwest::Url::parse_with_params(&api_endpoint, &params)?;
    let (status, body) = super::http::get_json(&client, url).await?;

    match status {
        reqwest::StatusCode::OK => {
            let lrclib_response = serde_json::from_value::<Response>(body)?;
            Ok(lrclib_response)
        }

        reqwest::StatusCode::BAD_REQUEST
        | reqwest::StatusCode::SERVICE_UNAVAILABLE
        | reqwest::StatusCode::INTERNAL_SERVER_ERROR => {
            let error = serde_json::from_value::<ResponseError>(body)?;
            Err(error.into())
        }

        _ => Err(ResponseError {
            status_code: None,
            error: "UnknownError".to_string(),
            message: "Unknown error happened".to_string(),
        }
        .into()),
    }
}
