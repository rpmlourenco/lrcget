use std::time::Duration;

use crate::utils::strip_timestamp;
use anyhow::Result;
use reqwest;
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawResponse {
    pub plain_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
    pub lyricsfile: Option<String>,
    instrumental: bool,
    lang: Option<String>,
    isrc: Option<String>,
    spotify_id: Option<String>,
    name: Option<String>,
    album_name: Option<String>,
    artist_name: Option<String>,
    release_date: Option<String>,
    duration: Option<f64>,
}

#[derive(Serialize)]
#[serde(tag = "type", content = "lyrics")]
pub enum Response {
    SyncedLyrics(String, String),
    UnsyncedLyrics(String),
    IsInstrumental,
    None,
}

impl Response {
    pub fn from_raw_response(lrclib_response: RawResponse) -> Response {
        match lrclib_response.synced_lyrics {
            Some(synced_lyrics) => {
                let plain_lyrics = match lrclib_response.plain_lyrics {
                    Some(plain_lyrics) => plain_lyrics,
                    None => strip_timestamp(&synced_lyrics),
                };
                Response::SyncedLyrics(synced_lyrics, plain_lyrics)
            }
            None => match lrclib_response.plain_lyrics {
                Some(unsynced_lyrics) => Response::UnsyncedLyrics(unsynced_lyrics),
                None => {
                    if lrclib_response.instrumental {
                        Response::IsInstrumental
                    } else {
                        Response::None
                    }
                }
            },
        }
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

async fn make_request(
    title: &str,
    album_name: Option<&str>,
    artist_name: &str,
    duration: f64,
    lrclib_instance: &str,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let mut params: Vec<(String, String)> = vec![
        ("artist_name".to_owned(), artist_name.to_owned()),
        ("track_name".to_owned(), title.to_owned()),
        ("duration".to_owned(), duration.round().to_string()),
    ];
    if let Some(album_name) = album_name.filter(|name| !name.is_empty()) {
        params.push(("album_name".to_owned(), album_name.to_owned()));
    }

    let version = env!("CARGO_PKG_VERSION");
    let user_agent = format!(
        "LRCGET v{} (https://github.com/tranxuanthang/lrcget)",
        version
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(user_agent)
        .build()?;
    let api_endpoint = format!("{}/api/get", lrclib_instance.trim_end_matches('/'));
    let url = reqwest::Url::parse_with_params(&api_endpoint, &params)?;
    super::http::get_json(&client, url).await
}

async fn request_raw_once(
    title: &str,
    album_name: Option<&str>,
    artist_name: &str,
    duration: f64,
    lrclib_instance: &str,
) -> Result<RawResponse> {
    let (status, body) = make_request(title, album_name, artist_name, duration, lrclib_instance).await?;

    match status {
        reqwest::StatusCode::OK => {
            let lrclib_response = serde_json::from_value::<RawResponse>(body)?;

            if lrclib_response.synced_lyrics.is_some()
                || lrclib_response.plain_lyrics.is_some()
                || lrclib_response.lyricsfile.is_some()
                || lrclib_response.instrumental
            {
                Ok(lrclib_response)
            } else {
                Err(ResponseError {
                    status_code: Some(404),
                    error: "NotFound".to_string(),
                    message: "There is no lyrics for this track".to_string(),
                }
                .into())
            }
        }

        reqwest::StatusCode::NOT_FOUND => Err(ResponseError {
            status_code: Some(404),
            error: "NotFound".to_string(),
            message: "There is no lyrics for this track".to_string(),
        }
        .into()),

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

fn artist_variants(artist_name: &str) -> Vec<String> {
    let separator = Regex::new(r"(?i)\b(?:and|plus)\b|[&+]").unwrap();
    if !separator.is_match(artist_name) {
        return Vec::new();
    }
    ["and", "&", "plus", "+"]
        .iter()
        .map(|replacement| {
            let padded = format!(" {replacement} ");
            separator
                .replace_all(artist_name, padded.as_str())
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|variant| variant != artist_name)
        .collect()
}

pub async fn request_raw(
    title: &str,
    album_name: &str,
    artist_name: &str,
    duration: f64,
    lrclib_instance: &str,
) -> Result<RawResponse> {
    match request_raw_once(title, Some(album_name), artist_name, duration, lrclib_instance).await {
        Ok(response) => return Ok(response),
        Err(error) if is_not_found(&error) => {}
        Err(error) => return Err(error),
    }

    let mut candidates = Vec::new();
    let mut artists = vec![artist_name.to_owned()];
    artists.extend(artist_variants(artist_name));
    artists.dedup();
    for artist in artists {
        let results = super::search::request(title, "", &artist, "", lrclib_instance).await?;
        candidates.extend(results.into_items());
    }
    if let Some(response) = select_fallback(candidates, title, album_name, artist_name, duration) {
        return Ok(response);
    }
    Err(ResponseError {
        status_code: Some(404),
        error: "NotFound".to_string(),
        message: "There is no lyrics for this track".to_string(),
    }.into())
}

fn normalize_artist(name: &str) -> String {
    let separator = Regex::new(r"(?i)\b(?:and|plus)\b|[&+]").unwrap();
    separator
        .replace_all(&name.to_lowercase(), " and ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn select_fallback(
    candidates: Vec<super::search::SearchItem>,
    title: &str,
    album_name: &str,
    artist_name: &str,
    duration: f64,
) -> Option<RawResponse> {
    let normalized_artist = normalize_artist(artist_name);
    let mut ranked = candidates.into_iter().filter_map(|item| {
        if !item.name.as_deref().is_some_and(|name| name.trim().eq_ignore_ascii_case(title.trim()))
            || !item.artist_name.as_deref().is_some_and(|name| normalize_artist(name) == normalized_artist)
        {
            return None;
        }
        let has_lyrics = item.plain_lyrics.as_ref().is_some_and(|text| !text.is_empty())
            || item.synced_lyrics.as_ref().is_some_and(|text| !text.is_empty())
            || item.lyricsfile.as_ref().is_some_and(|text| !text.is_empty())
            || item.instrumental;
        if !has_lyrics {
            return None;
        }
        let difference = item.duration.map(|value| (value - duration).abs());
        let tier = match difference {
            Some(_) if item.duration.unwrap().round() == duration.round() => 0,
            Some(value) if value < 3.0 => 1,
            _ if item.plain_lyrics.as_ref().is_some_and(|text| !text.is_empty()) => 2,
            _ => return None,
        };
        let album_mismatch = !item.album_name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(album_name));
        Some((tier, album_mismatch, difference.unwrap_or(f64::MAX), item))
    }).collect::<Vec<_>>();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.total_cmp(&b.2)));
    ranked.into_iter().next().map(|(tier, _, _, item)| RawResponse {
        plain_lyrics: item.plain_lyrics,
        synced_lyrics: if tier == 2 { None } else { item.synced_lyrics },
        lyricsfile: if tier == 2 { None } else { item.lyricsfile },
        instrumental: if tier == 2 { false } else { item.instrumental },
        lang: None,
        isrc: None,
        spotify_id: None,
        name: item.name,
        album_name: item.album_name,
        artist_name: item.artist_name,
        release_date: None,
        duration: item.duration,
    })
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ResponseError>().is_some_and(|error| error.status_code == Some(404))
}

pub async fn request(
    title: &str,
    album_name: &str,
    artist_name: &str,
    duration: f64,
    lrclib_instance: &str,
) -> Result<Response> {
    match request_raw(title, album_name, artist_name, duration, lrclib_instance).await {
        Ok(response) => Ok(Response::from_raw_response(response)),
        Err(error) if is_not_found(&error) => Ok(Response::None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{artist_variants, select_fallback};
    use crate::lrclib::search::SearchItem;

    fn candidate(duration: f64, artist: &str) -> SearchItem {
        SearchItem {
            id: 1,
            name: Some("Song".to_owned()),
            artist_name: Some(artist.to_owned()),
            album_name: Some("Other album".to_owned()),
            duration: Some(duration),
            instrumental: false,
            plain_lyrics: Some("plain".to_owned()),
            synced_lyrics: Some("[00:01.00] plain".to_owned()),
            lyricsfile: None,
        }
    }

    #[test]
    fn artist_separator_variants() {
        assert_eq!(artist_variants("A & B"), vec!["A and B", "A plus B", "A + B"]);
        assert!(artist_variants("The Band").is_empty());
        assert!(artist_variants("A plus B").contains(&"A & B".to_string()));
        assert!(artist_variants("A+B").contains(&"A and B".to_string()));
    }

    #[test]
    fn fallback_prefers_equal_duration_over_nearby_duration() {
        let result = select_fallback(
            vec![candidate(201.5, "A and B"), candidate(200.0, "A & B")],
            "Song", "Album", "A & B", 200.0,
        ).unwrap();
        assert_eq!(result.duration, Some(200.0));
        assert!(result.synced_lyrics.is_some());
    }

    #[test]
    fn fallback_accepts_under_three_seconds_then_plain_only() {
        let near = select_fallback(vec![candidate(202.9, "A plus B")], "Song", "Album", "A & B", 200.0).unwrap();
        assert!(near.synced_lyrics.is_some());
        let far = select_fallback(vec![candidate(203.0, "A plus B")], "Song", "Album", "A & B", 200.0).unwrap();
        assert_eq!(far.plain_lyrics.as_deref(), Some("plain"));
        assert!(far.synced_lyrics.is_none());
        assert!(far.lyricsfile.is_none());
    }

    #[test]
    fn fallback_rejects_unmatched_or_unusable_results() {
        let mut wrong_title = candidate(200.0, "A & B");
        wrong_title.name = Some("Different song".to_owned());
        let mut no_plain = candidate(203.0, "A & B");
        no_plain.plain_lyrics = None;
        assert!(select_fallback(vec![wrong_title, no_plain], "Song", "Album", "A & B", 200.0).is_none());
    }
}
