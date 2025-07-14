# app.py
from flask import Flask, render_template, jsonify
import feedparser
from bs4 import BeautifulSoup
import datetime
import pytz
import json
import re  # ADD THIS LINE
from urllib.parse import urlparse, parse_qs # ADD THIS LINE

app = Flask(__name__)

def get_youtube_video_id(url):
    """Extracts the YouTube video ID from a URL."""
    if not url:
        return None
    query = urlparse(url).query
    params = parse_qs(query)
    return params.get("v", [None])[0]

SITES = {
    "Author name": {"rss": "website.com/rss.xml"},
    "YouTuber name": {"rss": "https://www.youtube.com/feeds/videos.xml?channel_id=channelID", "type": "youtube"}
}

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

if __name__ == '__main__':
    app.run(debug=True)