use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use feed_rs::parser;
use reqwest::blocking::Client;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Manager, State, WebviewWindow};
use url::Url;

const DEFAULT_FEEDS: &str = include_str!(concat!(env!("OUT_DIR"), "/default-feeds.json"));

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    feeds_path: PathBuf,
    client: Client,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FeedSource {
    name: String,
    rss: String,
    #[serde(
        default = "default_source_type",
        rename = "type",
        skip_serializing_if = "is_default_source_type"
    )]
    source_type: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FeedEntry {
    id: String,
    author: String,
    title: String,
    link: String,
    description: String,
    #[serde(rename = "pubDate")]
    pub_date: String,
    is_summary: bool,
    source_type: String,
    video_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArchiveEntry {
    link: String,
    title: Option<String>,
    author: Option<String>,
    #[serde(rename = "pubDate")]
    pub_date: Option<String>,
    source_type: Option<String>,
}

fn default_source_type() -> String {
    "blog".to_string()
}

fn is_default_source_type(source_type: &str) -> bool {
    source_type == "blog"
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&app_data_dir)?;

            let feeds_path = app_data_dir.join("feeds.json");
            if !feeds_path.exists() {
                fs::write(&feeds_path, DEFAULT_FEEDS)?;
            }

            let db_path = app_data_dir.join("archive.sqlite3");
            init_db(&db_path)?;

            let client = Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent("GregsFeed/0.1 Tauri")
                .build()?;

            app.manage(AppState {
                db_path,
                feeds_path,
                client,
            });

            let main_window = app
                .get_webview_window("main")
                .ok_or("Could not find the main application window")?;
            load_bundled_ui(&main_window)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_feed,
            get_feed_sources,
            save_feed_sources,
            get_archived_ids,
            archive_article,
            reinstate_article,
            open_external_url,
            get_storage_info
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

#[tauri::command]
async fn get_feed_sources(state: State<'_, AppState>) -> Result<Vec<FeedSource>, String> {
    let feeds_path = state.feeds_path.clone();
    tauri::async_runtime::spawn_blocking(move || load_sources(&feeds_path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn save_feed_sources(
    state: State<'_, AppState>,
    sources: Vec<FeedSource>,
) -> Result<Vec<FeedSource>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || write_sources(&state, sources))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_feed(state: State<'_, AppState>) -> Result<Vec<FeedEntry>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || fetch_entries(&state))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_archived_ids(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || read_archived_ids(&db_path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn archive_article(
    state: State<'_, AppState>,
    entry: ArchiveEntry,
) -> Result<String, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || write_archived_article(&db_path, entry))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn reinstate_article(
    state: State<'_, AppState>,
    article_id: String,
) -> Result<String, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || delete_archived_article(&db_path, &article_id))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    let url = url.trim();
    let parsed = Url::parse(url).map_err(|error| format!("Invalid web URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Web view only supports http and https URLs.".to_string());
    }

    let status = Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .map_err(|error| format!("Could not open the default browser: {error}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("The default browser command exited with {status}."))
    }
}

#[cfg(target_os = "macos")]
fn load_bundled_ui(window: &WebviewWindow) -> tauri::Result<()> {
    let html = include_str!("../../templates/index.html").to_string();
    window.with_webview(move |webview| unsafe {
        use objc2_foundation::{NSString, NSURL};
        use objc2_web_kit::WKWebView;

        let view: &WKWebView = &*webview.inner().cast();
        let html = NSString::from_str(&html);
        let base_url =
            NSURL::URLWithString(&NSString::from_str("https://com.gregorycotton.gregs-feed/"))
                .expect("valid application identity URL");
        view.loadHTMLString_baseURL(&html, Some(&base_url));
    })
}

#[cfg(not(target_os = "macos"))]
fn load_bundled_ui(_window: &WebviewWindow) -> tauri::Result<()> {
    Ok(())
}

#[tauri::command]
fn get_storage_info(state: State<'_, AppState>) -> StorageInfo {
    StorageInfo {
        feeds_path: state.feeds_path.display().to_string(),
        db_path: state.db_path.display().to_string(),
    }
}

#[derive(Serialize)]
struct StorageInfo {
    feeds_path: String,
    db_path: String,
}

fn init_db(db_path: &Path) -> Result<(), String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    connection
        .execute(
            r#"
            CREATE TABLE IF NOT EXISTS archived_articles (
                article_id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT,
                author TEXT,
                published_at TEXT,
                archived_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                source_type TEXT
            )
            "#,
            [],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            r#"
            CREATE UNIQUE INDEX IF NOT EXISTS archived_articles_url_idx
            ON archived_articles (url)
            "#,
            [],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn load_sources(feeds_path: &Path) -> Result<Vec<FeedSource>, String> {
    let config = fs::read_to_string(feeds_path).map_err(|error| error.to_string())?;
    serde_json::from_str(&config).map_err(|error| format!("Could not parse feeds.json: {error}"))
}

fn write_sources(state: &AppState, sources: Vec<FeedSource>) -> Result<Vec<FeedSource>, String> {
    let mut cleaned_sources = Vec::new();

    for source in sources {
        let name = source.name.trim().to_string();
        let mut rss = source.rss.trim().to_string();
        let source_type = match source.source_type.trim().to_lowercase().as_str() {
            "" | "blog" => "blog".to_string(),
            "youtube" => "youtube".to_string(),
            other => return Err(format!("Unsupported source type: {other}")),
        };

        if name.is_empty() {
            return Err("Feed names cannot be empty.".to_string());
        }
        if rss.is_empty() {
            return Err(format!("{name} needs a feed URL."));
        }

        if source_type == "youtube" {
            rss = normalize_youtube_feed_url(&state.client, &rss)?;
        }

        validate_feed_url(&rss)?;

        cleaned_sources.push(FeedSource {
            name,
            rss,
            source_type,
        });
    }

    let serialized =
        serde_json::to_string_pretty(&cleaned_sources).map_err(|error| error.to_string())?;
    fs::write(&state.feeds_path, format!("{serialized}\n")).map_err(|error| error.to_string())?;
    Ok(cleaned_sources)
}

fn validate_feed_url(url: &str) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|error| format!("Invalid feed URL {url}: {error}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(format!("Feed URL must use http or https, not {scheme}.")),
    }
}

fn normalize_youtube_feed_url(client: &Client, input: &str) -> Result<String, String> {
    if input.starts_with("UC") && !input.contains('/') {
        return Ok(format!(
            "https://www.youtube.com/feeds/videos.xml?channel_id={input}"
        ));
    }

    let parsed =
        Url::parse(input).map_err(|error| format!("Invalid YouTube URL {input}: {error}"))?;
    let host = parsed.host_str().unwrap_or_default();
    if !(host.ends_with("youtube.com") || host == "youtu.be") {
        return Ok(input.to_string());
    }

    if parsed.path() == "/feeds/videos.xml"
        && parsed.query().unwrap_or_default().contains("channel_id=")
    {
        return Ok(input.to_string());
    }

    if let Some(channel_id) =
        parsed
            .path_segments()
            .and_then(|mut segments| match (segments.next(), segments.next()) {
                (Some("channel"), Some(channel_id)) => Some(channel_id.to_string()),
                _ => None,
            })
    {
        return Ok(format!(
            "https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}"
        ));
    }

    let page = client
        .get(input)
        .send()
        .map_err(|error| format!("Could not resolve YouTube channel URL: {error}"))?
        .error_for_status()
        .map_err(|error| format!("Could not resolve YouTube channel URL: {error}"))?
        .text()
        .map_err(|error| format!("Could not read YouTube channel page: {error}"))?;

    let channel_id = extract_youtube_channel_id(&page)
        .ok_or_else(|| "Could not find a YouTube channel ID on that page.".to_string())?;

    Ok(format!(
        "https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}"
    ))
}

fn extract_youtube_channel_id(page: &str) -> Option<String> {
    find_after_marker(page, "\"channelId\":\"")
        .or_else(|| find_after_marker(page, "/channel/"))
        .filter(|channel_id| channel_id.starts_with("UC"))
}

fn find_after_marker(page: &str, marker: &str) -> Option<String> {
    let start = page.find(marker)? + marker.len();
    let channel_id: String = page[start..]
        .chars()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || *character == '_' || *character == '-'
        })
        .collect();
    (!channel_id.is_empty()).then_some(channel_id)
}

fn fetch_entries(state: &AppState) -> Result<Vec<FeedEntry>, String> {
    let sources = load_sources(&state.feeds_path)?;
    let mut entries = Vec::new();

    for source in sources {
        match fetch_source_entries(state, &source) {
            Ok(mut source_entries) => entries.append(&mut source_entries),
            Err(error) => eprintln!("Could not fetch RSS for {}: {error}", source.name),
        }
    }

    entries.sort_by(|left, right| right.pub_date.cmp(&left.pub_date));
    Ok(entries)
}

fn fetch_source_entries(state: &AppState, source: &FeedSource) -> Result<Vec<FeedEntry>, String> {
    let response = state
        .client
        .get(&source.rss)
        .send()
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .bytes()
        .map_err(|error| error.to_string())?;

    let feed = parser::parse(Cursor::new(response)).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();

    for entry in feed.entries.into_iter().take(10) {
        let link = entry
            .links
            .first()
            .map(|link| link.href.clone())
            .unwrap_or_else(|| entry.id.clone());
        if link.trim().is_empty() {
            continue;
        }

        let title = entry
            .title
            .as_ref()
            .map(|title| title.content.clone())
            .unwrap_or_else(|| "Untitled".to_string());
        let description = entry
            .content
            .as_ref()
            .and_then(|content| content.body.clone())
            .or_else(|| {
                entry
                    .summary
                    .as_ref()
                    .map(|summary| summary.content.clone())
            })
            .unwrap_or_default();
        let pub_date = entry
            .published
            .or(entry.updated)
            .map(|date| date.to_rfc3339())
            .unwrap_or_default();
        let source_type = source.source_type.clone();
        let is_summary = source_type != "youtube" && looks_like_summary(&description);
        let video_id = if source_type == "youtube" {
            get_youtube_video_id(&link)
        } else {
            None
        };

        entries.push(FeedEntry {
            id: article_id(&link),
            author: source.name.clone(),
            title,
            link,
            description,
            pub_date,
            is_summary,
            source_type,
            video_id,
        });
    }

    Ok(entries)
}

fn looks_like_summary(description: &str) -> bool {
    let lower = description.to_lowercase();
    description.trim_end().ends_with("...")
        || lower.contains("read more")
        || lower.contains("continue reading")
}

fn get_youtube_video_id(link: &str) -> Option<String> {
    let url = Url::parse(link).ok()?;
    url.query_pairs()
        .find(|(key, _)| key == "v")
        .map(|(_, value)| value.into_owned())
}

fn article_id(link: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(link.trim().as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_archived_ids(db_path: &Path) -> Result<Vec<String>, String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare("SELECT article_id FROM archived_articles")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn write_archived_article(db_path: &Path, entry: ArchiveEntry) -> Result<String, String> {
    let link = entry.link.trim();
    if link.is_empty() {
        return Err("Archived articles require a link.".to_string());
    }

    let id = article_id(link);
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    connection
        .execute(
            r#"
            INSERT INTO archived_articles (
                article_id, url, title, author, published_at, source_type
            )
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(article_id) DO UPDATE SET
                url = excluded.url,
                title = excluded.title,
                author = excluded.author,
                published_at = excluded.published_at,
                source_type = excluded.source_type
            "#,
            params![
                id,
                link,
                entry.title,
                entry.author,
                entry.pub_date,
                entry.source_type.unwrap_or_else(default_source_type)
            ],
        )
        .map_err(|error| error.to_string())?;

    Ok(article_id(link))
}

fn delete_archived_article(db_path: &Path, article_id: &str) -> Result<String, String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM archived_articles WHERE article_id = ?",
            params![article_id],
        )
        .map_err(|error| error.to_string())?;
    Ok(article_id.to_string())
}
