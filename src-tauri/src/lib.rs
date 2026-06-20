use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use feed_rs::parser;
use reqwest::{
    blocking::Client,
    header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED},
    StatusCode,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
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

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ArticleCursor {
    published_ts: i64,
    article_id: String,
}

#[derive(Serialize)]
struct ArticlePage {
    entries: Vec<FeedEntry>,
    next_cursor: Option<ArticleCursor>,
}

#[derive(Serialize)]
struct SyncResult {
    updated_articles: usize,
    unchanged_feeds: usize,
    failed_feeds: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct FeedValidator {
    etag: Option<String>,
    last_modified: Option<String>,
}

struct FetchedFeed {
    source: FeedSource,
    entries: Vec<CachedFeedEntry>,
    etag: Option<String>,
    last_modified: Option<String>,
}

struct CachedFeedEntry {
    entry: FeedEntry,
    published_ts: i64,
}

enum FeedSyncOutcome {
    Updated(FetchedFeed),
    NotModified(FeedSource),
    Failed(FeedSource, String),
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
            get_articles_page,
            refresh_feeds,
            get_feed_sources,
            save_feed_sources,
            archive_article,
            reinstate_article,
            delete_article,
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
async fn get_articles_page(
    state: State<'_, AppState>,
    archived: bool,
    cursor: Option<ArticleCursor>,
    query: Option<String>,
    limit: Option<u32>,
) -> Result<ArticlePage, String> {
    let db_path = state.db_path.clone();
    let limit = limit.unwrap_or(20).clamp(1, 200) as usize;
    tauri::async_runtime::spawn_blocking(move || {
        read_articles_page(&db_path, archived, cursor, query, limit)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn refresh_feeds(state: State<'_, AppState>) -> Result<SyncResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || sync_feeds(&state))
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
async fn delete_article(state: State<'_, AppState>, article_id: String) -> Result<String, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || permanently_delete_article(&db_path, &article_id))
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
    let mut connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    connection
        .execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS articles (
                article_id TEXT PRIMARY KEY,
                url TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                author TEXT NOT NULL,
                description TEXT NOT NULL,
                published_at TEXT NOT NULL,
                published_ts INTEGER NOT NULL,
                is_summary INTEGER NOT NULL DEFAULT 0,
                source_type TEXT NOT NULL DEFAULT 'blog',
                video_id TEXT,
                feed_url TEXT NOT NULL,
                last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS articles_date_idx
            ON articles (published_ts DESC, article_id DESC);

            CREATE INDEX IF NOT EXISTS articles_last_seen_idx
            ON articles (last_seen_at);

            CREATE TABLE IF NOT EXISTS feed_state (
                feed_url TEXT PRIMARY KEY,
                etag TEXT,
                last_modified TEXT,
                last_checked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS deleted_articles (
                article_id TEXT PRIMARY KEY,
                deleted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS deleted_articles_date_idx
            ON deleted_articles (deleted_at);
            "#,
        )
        .map_err(|error| error.to_string())?;
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
    prune_cache(&mut connection)
}

fn prune_cache(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;

    transaction
        .execute(
            r#"
            INSERT INTO deleted_articles (article_id, deleted_at)
            SELECT article_id, CURRENT_TIMESTAMP
            FROM archived_articles
            WHERE archived_at <= datetime('now', '-7 days')
            ON CONFLICT(article_id) DO UPDATE SET deleted_at = excluded.deleted_at
            "#,
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            r#"
            DELETE FROM articles
            WHERE article_id IN (
                SELECT article_id
                FROM archived_articles
                WHERE archived_at <= datetime('now', '-7 days')
            )
            "#,
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM archived_articles WHERE archived_at <= datetime('now', '-7 days')",
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            r#"
            DELETE FROM articles
            WHERE last_seen_at <= datetime('now', '-90 days')
              AND article_id NOT IN (SELECT article_id FROM archived_articles)
            "#,
            [],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM deleted_articles WHERE deleted_at <= datetime('now', '-90 days')",
            [],
        )
        .map_err(|error| error.to_string())?;

    transaction.commit().map_err(|error| error.to_string())
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

fn read_articles_page(
    db_path: &Path,
    archived: bool,
    cursor: Option<ArticleCursor>,
    query: Option<String>,
    limit: usize,
) -> Result<ArticlePage, String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    let archived_flag = i64::from(archived);
    let normalized_query = query
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let cursor_ts = cursor.as_ref().map(|value| value.published_ts);
    let cursor_id = cursor.as_ref().map(|value| value.article_id.clone());
    let mut statement = connection
        .prepare(
            r#"
            SELECT
                a.article_id,
                a.author,
                a.title,
                a.url,
                a.description,
                a.published_at,
                a.is_summary,
                a.source_type,
                a.video_id,
                a.published_ts
            FROM articles a
            LEFT JOIN archived_articles archived ON archived.article_id = a.article_id
            LEFT JOIN deleted_articles deleted ON deleted.article_id = a.article_id
            WHERE deleted.article_id IS NULL
              AND (
                    (?1 = 1 AND archived.article_id IS NOT NULL)
                 OR (?1 = 0 AND archived.article_id IS NULL)
              )
              AND (
                    ?2 IS NULL
                 OR a.title LIKE '%' || ?2 || '%' COLLATE NOCASE
                 OR a.author LIKE '%' || ?2 || '%' COLLATE NOCASE
                 OR a.description LIKE '%' || ?2 || '%' COLLATE NOCASE
              )
              AND (
                    ?3 IS NULL
                 OR a.published_ts < ?3
                 OR (a.published_ts = ?3 AND a.article_id < ?4)
              )
            ORDER BY a.published_ts DESC, a.article_id DESC
            LIMIT ?5
            "#,
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![
                archived_flag,
                normalized_query,
                cursor_ts,
                cursor_id,
                (limit + 1) as i64
            ],
            |row| {
                Ok((
                    FeedEntry {
                        id: row.get(0)?,
                        author: row.get(1)?,
                        title: row.get(2)?,
                        link: row.get(3)?,
                        description: row.get(4)?,
                        pub_date: row.get(5)?,
                        is_summary: row.get::<_, i64>(6)? != 0,
                        source_type: row.get(7)?,
                        video_id: row.get(8)?,
                    },
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .map_err(|error| error.to_string())?;

    let mut cached_rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let has_more = cached_rows.len() > limit;
    cached_rows.truncate(limit);
    let next_cursor = if has_more {
        cached_rows
            .last()
            .map(|(entry, published_ts)| ArticleCursor {
                published_ts: *published_ts,
                article_id: entry.id.clone(),
            })
    } else {
        None
    };

    Ok(ArticlePage {
        entries: cached_rows.into_iter().map(|(entry, _)| entry).collect(),
        next_cursor,
    })
}

fn sync_feeds(state: &AppState) -> Result<SyncResult, String> {
    let sources = load_sources(&state.feeds_path)?;
    let validators = read_feed_validators(&state.db_path)?;
    let outcomes = thread::scope(|scope| {
        let handles = sources
            .into_iter()
            .map(|source| {
                let client = state.client.clone();
                let validator = validators.get(&source.rss).cloned().unwrap_or_default();
                scope.spawn(move || fetch_source_entries(&client, source, validator))
            })
            .collect::<Vec<_>>();

        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| panic!("feed refresh worker panicked"))
            })
            .collect::<Vec<_>>()
    });

    let mut connection = Connection::open(&state.db_path).map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    let mut updated_articles = 0;
    let mut unchanged_feeds = 0;
    let mut failed_feeds = Vec::new();

    for outcome in outcomes {
        match outcome {
            FeedSyncOutcome::Updated(feed) => {
                updated_articles += write_fetched_feed(&transaction, feed)?;
            }
            FeedSyncOutcome::NotModified(source) => {
                unchanged_feeds += 1;
                mark_feed_checked(&transaction, &source.rss)?;
            }
            FeedSyncOutcome::Failed(source, error) => {
                eprintln!("Could not fetch RSS for {}: {error}", source.name);
                failed_feeds.push(source.name);
            }
        }
    }

    transaction.commit().map_err(|error| error.to_string())?;
    prune_cache(&mut connection)?;

    Ok(SyncResult {
        updated_articles,
        unchanged_feeds,
        failed_feeds,
    })
}

fn read_feed_validators(db_path: &Path) -> Result<HashMap<String, FeedValidator>, String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    let mut statement = connection
        .prepare("SELECT feed_url, etag, last_modified FROM feed_state")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                FeedValidator {
                    etag: row.get(1)?,
                    last_modified: row.get(2)?,
                },
            ))
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|error| error.to_string())
}

fn fetch_source_entries(
    client: &Client,
    source: FeedSource,
    validator: FeedValidator,
) -> FeedSyncOutcome {
    match try_fetch_source_entries(client, &source, validator) {
        Ok(Some(feed)) => FeedSyncOutcome::Updated(feed),
        Ok(None) => FeedSyncOutcome::NotModified(source),
        Err(error) => FeedSyncOutcome::Failed(source, error),
    }
}

fn try_fetch_source_entries(
    client: &Client,
    source: &FeedSource,
    validator: FeedValidator,
) -> Result<Option<FetchedFeed>, String> {
    let mut request = client.get(&source.rss);
    if let Some(etag) = validator.etag {
        request = request.header(IF_NONE_MATCH, etag);
    }
    if let Some(last_modified) = validator.last_modified {
        request = request.header(IF_MODIFIED_SINCE, last_modified);
    }

    let response = request.send().map_err(|error| error.to_string())?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let last_modified = response
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let response = response.bytes().map_err(|error| error.to_string())?;
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
        let published = entry.published.or(entry.updated);
        let pub_date = published.map(|date| date.to_rfc3339()).unwrap_or_default();
        let published_ts = published
            .map(|date| date.timestamp())
            .unwrap_or_else(current_unix_timestamp);
        let source_type = source.source_type.clone();
        let is_summary = source_type != "youtube" && looks_like_summary(&description);
        let video_id = if source_type == "youtube" {
            get_youtube_video_id(&link)
        } else {
            None
        };

        entries.push(CachedFeedEntry {
            entry: FeedEntry {
                id: article_id(&link),
                author: source.name.clone(),
                title,
                link,
                description,
                pub_date,
                is_summary,
                source_type,
                video_id,
            },
            published_ts,
        });
    }

    Ok(Some(FetchedFeed {
        source: source.clone(),
        entries,
        etag,
        last_modified,
    }))
}

fn write_fetched_feed(transaction: &Transaction<'_>, feed: FetchedFeed) -> Result<usize, String> {
    let mut updated = 0;
    for cached in feed.entries {
        let is_deleted = transaction
            .query_row(
                "SELECT 1 FROM deleted_articles WHERE article_id = ?",
                params![cached.entry.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .is_some();
        if is_deleted {
            continue;
        }

        transaction
            .execute(
                r#"
                INSERT INTO articles (
                    article_id, url, title, author, description, published_at,
                    published_ts, is_summary, source_type, video_id, feed_url, last_seen_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
                ON CONFLICT(article_id) DO UPDATE SET
                    url = excluded.url,
                    title = excluded.title,
                    author = excluded.author,
                    description = excluded.description,
                    published_at = excluded.published_at,
                    published_ts = excluded.published_ts,
                    is_summary = excluded.is_summary,
                    source_type = excluded.source_type,
                    video_id = excluded.video_id,
                    feed_url = excluded.feed_url,
                    last_seen_at = CURRENT_TIMESTAMP
                "#,
                params![
                    cached.entry.id,
                    cached.entry.link,
                    cached.entry.title,
                    cached.entry.author,
                    cached.entry.description,
                    cached.entry.pub_date,
                    cached.published_ts,
                    i64::from(cached.entry.is_summary),
                    cached.entry.source_type,
                    cached.entry.video_id,
                    feed.source.rss,
                ],
            )
            .map_err(|error| error.to_string())?;
        updated += 1;
    }

    transaction
        .execute(
            r#"
            INSERT INTO feed_state (feed_url, etag, last_modified, last_checked_at)
            VALUES (?, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(feed_url) DO UPDATE SET
                etag = excluded.etag,
                last_modified = excluded.last_modified,
                last_checked_at = CURRENT_TIMESTAMP
            "#,
            params![feed.source.rss, feed.etag, feed.last_modified],
        )
        .map_err(|error| error.to_string())?;
    Ok(updated)
}

fn mark_feed_checked(transaction: &Transaction<'_>, feed_url: &str) -> Result<(), String> {
    transaction
        .execute(
            "UPDATE feed_state SET last_checked_at = CURRENT_TIMESTAMP WHERE feed_url = ?",
            params![feed_url],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
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
                source_type = excluded.source_type,
                archived_at = CURRENT_TIMESTAMP
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

fn permanently_delete_article(db_path: &Path, article_id: &str) -> Result<String, String> {
    let mut connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            r#"
            INSERT INTO deleted_articles (article_id, deleted_at)
            VALUES (?, CURRENT_TIMESTAMP)
            ON CONFLICT(article_id) DO UPDATE SET deleted_at = excluded.deleted_at
            "#,
            params![article_id],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM archived_articles WHERE article_id = ?",
            params![article_id],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "DELETE FROM articles WHERE article_id = ?",
            params![article_id],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(article_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_db(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gregs-feed-{name}-{}-{}.sqlite3",
            std::process::id(),
            current_unix_timestamp()
        ))
    }

    fn insert_article(connection: &Connection, id: &str, timestamp: i64) {
        connection
            .execute(
                r#"
                INSERT INTO articles (
                    article_id, url, title, author, description, published_at,
                    published_ts, is_summary, source_type, feed_url
                )
                VALUES (?, ?, ?, 'Author', '', '', ?, 0, 'blog', 'https://example.com/feed')
                "#,
                params![id, format!("https://example.com/{id}"), id, timestamp],
            )
            .unwrap();
    }

    #[test]
    fn pages_feed_and_archive_in_date_order() {
        let db_path = temporary_db("paging");
        init_db(&db_path).unwrap();
        let connection = Connection::open(&db_path).unwrap();
        insert_article(&connection, "newest", 3);
        insert_article(&connection, "archived", 2);
        insert_article(&connection, "oldest", 1);
        connection
            .execute(
                "INSERT INTO archived_articles (article_id, url) VALUES ('archived', 'https://example.com/archived')",
                [],
            )
            .unwrap();

        let first = read_articles_page(&db_path, false, None, None, 1).unwrap();
        assert_eq!(first.entries[0].id, "newest");
        let second = read_articles_page(&db_path, false, first.next_cursor, None, 1).unwrap();
        assert_eq!(second.entries[0].id, "oldest");
        let archive = read_articles_page(&db_path, true, None, None, 20).unwrap();
        assert_eq!(archive.entries[0].id, "archived");

        drop(connection);
        fs::remove_file(db_path).ok();
    }

    #[test]
    fn expired_archive_is_deleted_and_tombstoned() {
        let db_path = temporary_db("expiry");
        init_db(&db_path).unwrap();
        let mut connection = Connection::open(&db_path).unwrap();
        insert_article(&connection, "expired", 1);
        connection
            .execute(
                r#"
                INSERT INTO archived_articles (article_id, url, archived_at)
                VALUES ('expired', 'https://example.com/expired', datetime('now', '-8 days'))
                "#,
                [],
            )
            .unwrap();

        prune_cache(&mut connection).unwrap();
        let article_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE article_id = 'expired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let archive_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM archived_articles WHERE article_id = 'expired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let tombstone_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM deleted_articles WHERE article_id = 'expired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((article_count, archive_count, tombstone_count), (0, 0, 1));

        let transaction = connection.transaction().unwrap();
        write_fetched_feed(
            &transaction,
            FetchedFeed {
                source: FeedSource {
                    name: "Author".to_string(),
                    rss: "https://example.com/feed".to_string(),
                    source_type: "blog".to_string(),
                },
                entries: vec![CachedFeedEntry {
                    entry: FeedEntry {
                        id: "expired".to_string(),
                        author: "Author".to_string(),
                        title: "Expired".to_string(),
                        link: "https://example.com/expired".to_string(),
                        description: String::new(),
                        pub_date: String::new(),
                        is_summary: false,
                        source_type: "blog".to_string(),
                        video_id: None,
                    },
                    published_ts: 1,
                }],
                etag: None,
                last_modified: None,
            },
        )
        .unwrap();
        transaction.commit().unwrap();
        let reinserted_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE article_id = 'expired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reinserted_count, 0);

        drop(connection);
        fs::remove_file(db_path).ok();
    }
}
