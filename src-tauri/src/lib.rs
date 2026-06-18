use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::Duration,
};

use feed_rs::parser;
use reqwest::blocking::Client;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Manager, State};
use url::Url;

const DEFAULT_FEEDS: &str = include_str!(concat!(env!("OUT_DIR"), "/default-feeds.json"));

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    feeds_path: PathBuf,
    client: Client,
}

#[derive(Debug, Deserialize)]
struct FeedSource {
    name: String,
    rss: String,
    #[serde(default = "default_source_type", rename = "type")]
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_feed,
            get_archived_ids,
            archive_article,
            reinstate_article,
            get_storage_info
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
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
