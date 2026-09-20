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
    #[serde(default)]
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

        _ => Err(super::http::api_error(status, &body)),
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

fn title_without_parentheses(title: &str) -> Option<String> {
    let parentheses = Regex::new(r"\([^)]*\)").unwrap();
    if !parentheses.is_match(title) {
        return None;
    }
    let stripped = parentheses
        .replace_all(title, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!stripped.is_empty() && stripped != title).then_some(stripped)
}

pub async fn request_raw(
    title: &str,
    album_name: &str,
    artist_name: &str,
    duration: f64,
    lrclib_instance: &str,
) -> Result<RawResponse> {
    let mut search_error = None;
    let exact = match request_raw_once(title, Some(album_name), artist_name, duration, lrclib_instance).await {
        Ok(response) if has_synced_lyrics(response.synced_lyrics.as_deref(), response.lyricsfile.as_deref()) || response.instrumental => return Ok(response),
        Ok(response) => Some(response),
        Err(error) if is_not_found(&error) => None,
        Err(error) if error.downcast_ref::<super::http::ApiError>().is_some_and(|error| error.status_code == 400) => {
            search_error = Some(error);
            None
        }
        Err(error) => return Err(error),
    };

    let mut candidates = Vec::new();
    let mut artists = vec![artist_name.to_owned()];
    artists.extend(artist_variants(artist_name));
    artists.dedup();
    for artist in &artists {
        match super::search::request(title, "", artist, "", lrclib_instance).await {
            Ok(results) => candidates.extend(results.into_items()),
            Err(error) => search_error = Some(error),
        }
    }
    let exact_synced_candidate = candidates.iter().any(|item| {
        duration_tier(item.duration, duration) == 0
            && has_synced_lyrics(item.synced_lyrics.as_deref(), item.lyricsfile.as_deref())
            && item.name.as_deref().is_some_and(|name| normalize_name(name) == normalize_name(title))
            && item.artist_name.as_deref().is_some_and(|name| normalize_artist(name) == normalize_artist(artist_name))
    });
    if let Some(stripped_title) = title_without_parentheses(title).filter(|_| !exact_synced_candidate) {
        for artist in &artists {
            match super::search::request(&stripped_title, "", artist, "", lrclib_instance).await {
                Ok(results) => candidates.extend(results.into_items()),
                Err(error) => search_error = Some(error),
            }
        }
    }
    let mut fallback = select_fallback(candidates, title, album_name, artist_name, duration);
    let normalized_title = normalize_name(title);
    if fallback.is_none() && !normalized_title.is_empty() && !title.trim().eq_ignore_ascii_case(&normalized_title) {
        match super::search::request(&normalized_title, "", artist_name, "", lrclib_instance).await {
            Ok(results) => fallback = select_fallback(results.into_items(), title, album_name, artist_name, duration),
            Err(error) => search_error = Some(error),
        }
    }
    if let Some(response) = choose_response(exact, fallback, duration) {
        return Ok(response);
    }
    if let Some(error) = search_error {
        return Err(error);
    }
    Err(ResponseError {
        status_code: Some(404),
        error: "NotFound".to_string(),
        message: "There is no lyrics for this track".to_string(),
    }.into())
}

fn normalize_name(name: &str) -> String {
    crate::utils::prepare_input(name)
        .chars()
        .map(|character| if character.is_alphanumeric() { character } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_artist(name: &str) -> String {
    let separator = Regex::new(r"(?i)\b(?:and|plus)\b|[&+]").unwrap();
    normalize_name(&separator.replace_all(name, " and "))
}

fn has_synced_lyrics(synced: Option<&str>, lyricsfile: Option<&str>) -> bool {
    synced.is_some_and(|text| !text.is_empty())
        || lyricsfile.is_some_and(|text| {
            crate::lyricsfile::lyrics_presence_from_lyricsfile(text)
                .is_ok_and(|presence| presence.has_synced_lyrics)
        })
}

fn duration_tier(candidate: Option<f64>, wanted: f64) -> u8 {
    match candidate {
        Some(value) if value.round() == wanted.round() => 0,
        Some(value) if (value - wanted).abs() < 3.0 => 1,
        _ => 2,
    }
}

fn choose_response(exact: Option<RawResponse>, fallback: Option<RawResponse>, duration: f64) -> Option<RawResponse> {
    match (exact, fallback) {
        (Some(exact), Some(fallback))
            if has_synced_lyrics(fallback.synced_lyrics.as_deref(), fallback.lyricsfile.as_deref())
                && duration_tier(fallback.duration, duration) <= 1
                && duration_tier(fallback.duration, duration) <= duration_tier(exact.duration, duration) => Some(fallback),
        (Some(exact), _) => Some(exact),
        (None, fallback) => fallback,
    }
}

fn select_fallback(
    candidates: Vec<super::search::SearchItem>,
    title: &str,
    album_name: &str,
    artist_name: &str,
    duration: f64,
) -> Option<RawResponse> {
    let normalized_artist = normalize_artist(artist_name);
    let mut normalized_titles = vec![normalize_name(title)];
    if let Some(stripped) = title_without_parentheses(title) {
        normalized_titles.push(normalize_name(&stripped));
    }
    if normalized_titles[0].is_empty() || normalized_artist.is_empty() {
        return None;
    }
    let mut ranked = candidates.into_iter().filter_map(|item| {
        let title_rank = item.name.as_deref().and_then(|name| {
            let normalized = normalize_name(name);
            normalized_titles.iter().position(|title| title == &normalized)
        })?;
        if !item.artist_name.as_deref().is_some_and(|name| normalize_artist(name) == normalized_artist) {
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
        let tier = duration_tier(item.duration, duration);
        if tier == 2 && !item.plain_lyrics.as_ref().is_some_and(|text| !text.is_empty()) {
            return None;
        }
        let has_synced = has_synced_lyrics(item.synced_lyrics.as_deref(), item.lyricsfile.as_deref());
        let album_mismatch = !item.album_name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(album_name));
        Some((tier, !has_synced, title_rank, album_mismatch, difference.unwrap_or(f64::MAX), item))
    }).collect::<Vec<_>>();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)).then_with(|| a.3.cmp(&b.3)).then_with(|| a.4.total_cmp(&b.4)));
    ranked.into_iter().next().map(|(tier, _, _, _, _, item)| RawResponse {
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
    use super::{artist_variants, choose_response, select_fallback, title_without_parentheses, RawResponse};
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

    #[test]
    fn synced_match_in_another_album_replaces_plain_match() {
        let plain = RawResponse {
            plain_lyrics: Some("plain".to_owned()),
            synced_lyrics: None,
            lyricsfile: None,
            instrumental: false,
            lang: None, isrc: None, spotify_id: None,
            name: Some("Time Is on Your Side".to_owned()),
            album_name: Some("Boogie Wonderland: The Best Of".to_owned()),
            artist_name: Some("Earth, Wind & Fire".to_owned()),
            release_date: None,
            duration: Some(222.506667),
        };
        let mut synced = candidate(222.0, "Earth Wind & Fire");
        synced.name = Some("Time Is On Your Side".to_owned());
        let mut plain_in_local_album = candidate(222.0, "Earth, Wind & Fire");
        plain_in_local_album.name = Some("Time Is On Your Side".to_owned());
        plain_in_local_album.album_name = Some("Boogie Wonderland: The Best Of".to_owned());
        plain_in_local_album.synced_lyrics = None;
        let fallback = select_fallback(
            vec![plain_in_local_album, synced], "Time Is on Your Side", "Boogie Wonderland: The Best Of", "Earth, Wind & Fire", 222.0,
        );
        let chosen = choose_response(Some(plain), fallback, 222.0).unwrap();
        assert_eq!(chosen.duration, Some(222.0));
        assert!(chosen.synced_lyrics.is_some());
    }

    #[test]
    fn apostrophe_styles_match_without_changing_the_song() {
        let mut item = candidate(200.0, "Singer");
        item.name = Some("Don’t Stop".to_owned());
        assert!(select_fallback(vec![item], "Don't Stop", "Album", "Singer", 200.0).is_some());
    }

    #[test]
    fn parenthetical_title_has_a_stripped_fallback() {
        assert_eq!(title_without_parentheses("Song (Live) (Remastered)"), Some("Song".to_owned()));
        assert_eq!(title_without_parentheses("Song"), None);
        let result = select_fallback(vec![candidate(200.0, "Singer")], "Song (Live)", "Album", "Singer", 200.0);
        assert!(result.is_some());
    }
}
