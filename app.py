# app.py
import os
import datetime
import hashlib
import re
import sqlite3
from pathlib import Path
from urllib.parse import urlparse, parse_qs

from flask import Flask, render_template, jsonify, request
import feedparser
from bs4 import BeautifulSoup
import pytz
from dotenv import load_dotenv

app = Flask(__name__)
load_dotenv()
DB_PATH = Path(os.getenv("RSS_FEED_DB", Path(app.root_path) / "archive.sqlite3"))

def get_youtube_video_id(url):
    """Extracts the YouTube video ID from a URL."""
    if not url:
        return None
    query = urlparse(url).query
    params = parse_qs(query)
    return params.get("v", [None])[0]

def get_article_id(url):
    return hashlib.sha256(url.strip().encode("utf-8")).hexdigest()

def get_db():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    connection = sqlite3.connect(DB_PATH)
    connection.row_factory = sqlite3.Row
    return connection

def init_db():
    with get_db() as db:
        db.execute(
            """
            CREATE TABLE IF NOT EXISTS archived_articles (
                article_id TEXT PRIMARY KEY,
                url TEXT NOT NULL,
                title TEXT,
                author TEXT,
                published_at TEXT,
                archived_at TEXT NOT NULL,
                source_type TEXT
            )
            """
        )
        db.execute(
            """
            CREATE UNIQUE INDEX IF NOT EXISTS archived_articles_url_idx
            ON archived_articles (url)
            """
        )

def get_archived_ids():
    with get_db() as db:
        rows = db.execute("SELECT article_id FROM archived_articles").fetchall()
    return [row["article_id"] for row in rows]

def archive_article(entry):
    url = entry.get("link", "").strip()
    if not url:
        raise ValueError("Archived articles require a link.")

    article_id = get_article_id(url)
    archived_at = datetime.datetime.now(datetime.timezone.utc).isoformat()

    with get_db() as db:
        db.execute(
            """
            INSERT INTO archived_articles (
                article_id, url, title, author, published_at, archived_at, source_type
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(article_id) DO UPDATE SET
                url = excluded.url,
                title = excluded.title,
                author = excluded.author,
                published_at = excluded.published_at,
                source_type = excluded.source_type
            """,
            (
                article_id,
                url,
                entry.get("title"),
                entry.get("author"),
                entry.get("pubDate"),
                archived_at,
                entry.get("source_type", "blog"),
            ),
        )

    return article_id

def reinstate_article(article_id):
    with get_db() as db:
        db.execute(
            "DELETE FROM archived_articles WHERE article_id = ?",
            (article_id,),
        )

def load_sites():
    sites = {}
    site_indices = sorted(
        {
            int(match.group(1))
            for key in os.environ
            if (match := re.fullmatch(r"RSS_SITE_(\d+)_(NAME|RSS|TYPE)", key))
        }
    )

    for index in site_indices:
        prefix = f"RSS_SITE_{index}_"
        name = os.getenv(f"{prefix}NAME")
        rss_url = os.getenv(f"{prefix}RSS")
        source_type = os.getenv(f"{prefix}TYPE", "").strip()

        if not name or not rss_url:
            print(f"Skipping incomplete RSS site config at RSS_SITE_{index}.")
            continue

        source = {"rss": rss_url}
        if source_type:
            source["type"] = source_type

        sites[name] = source

    if not sites:
        print("No RSS sites configured. Copy .env.example to .env and add RSS_SITE entries.")

    return sites


SITES = load_sites()
init_db()

def fetch_entries():
    all_entries = []
    for name, source in SITES.items():
        if "rss" in source:
            print(f"Fetching RSS feed for {name}...")
            try:
                feed = feedparser.parse(source["rss"])
                for entry in feed.entries[:10]:
                    description = entry.summary
                    if hasattr(entry, 'content'):
                        description = entry.content[0].value
                    
                    pubDate_str = ""
                    if entry.get("published_parsed"):
                         dt_aware = datetime.datetime(*entry.published_parsed[:6], tzinfo=pytz.utc)
                         pubDate_str = dt_aware.isoformat()

                    entry_data = {
                        "id": get_article_id(entry.link),
                        "author": name, "title": entry.title, "link": entry.link,
                        "description": description, "pubDate": pubDate_str,
                        "is_summary": False
                    }
                    
                    source_type = source.get("type", "blog")
                    entry_data["source_type"] = source_type

                    if source_type == "youtube":
                        entry_data["video_id"] = get_youtube_video_id(entry.link)
                    else:
                        soup = BeautifulSoup(description, 'lxml')
                        text_content = soup.get_text().strip()
                        if text_content.endswith('...') or soup.find('a', string=re.compile(r'read more|continue reading', re.I)):
                            entry_data["is_summary"] = True
                    
                    all_entries.append(entry_data)
            except Exception as e:
                print(f"Could not fetch RSS for {name}: {e}")

    all_entries.sort(key=lambda x: x['pubDate'] or '1970-01-01T00:00:00+00:00', reverse=True)
    return all_entries

@app.route('/')
def index():
    return render_template('index.html')

@app.route('/api/feed')
def api_feed():
    entries = fetch_entries()
    return jsonify(entries)

@app.route('/api/archive')
def api_archive():
    return jsonify({"archived_ids": get_archived_ids()})

@app.route('/api/archive', methods=['POST'])
def api_archive_article():
    entry = request.get_json(silent=True) or {}

    try:
        article_id = archive_article(entry)
    except ValueError as error:
        return jsonify({"error": str(error)}), 400

    return jsonify({"article_id": article_id})

@app.route('/api/archive/<article_id>', methods=['DELETE'])
def api_reinstate_article(article_id):
    reinstate_article(article_id)
    return jsonify({"article_id": article_id})

@app.route('/api/health')
def api_health():
    return jsonify({"ok": True, "sites": len(SITES)})

if __name__ == '__main__':
    host = os.getenv("RSS_FEED_HOST", "127.0.0.1")
    port = int(os.getenv("RSS_FEED_PORT", "5000"))
    debug = os.getenv("RSS_FEED_DEBUG") == "1"

    app.run(host=host, port=port, debug=debug, use_reloader=False)
