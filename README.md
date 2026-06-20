# Mouser

Mouser is a personal RSS and YouTube reader packaged as a lightweight Tauri desktop app for macOS. It uses the operating system's native WebView and a Rust backend; it does not run a local HTTP server or require the project directory at runtime.

## Features

- RSS, Atom, Substack, and YouTube channel feeds
- Date-ordered feed and archive views
- SQLite-backed article cache with pagination
- Concurrent background feed refresh
- Search across cached article titles, authors, and descriptions
- Inline YouTube playback in the native WebView
- In-app feed and theme management

## Loading articles

On launch, Mouser immediately queries the newest 20 cached articles from SQLite. It then refreshes every configured feed concurrently in the background and updates the visible page when synchronization finishes. Each feed response contributes up to 10 recent entries to the cache (first launch needs to download the feeds before it can show articles). Later launches can display cached articles without waiting for the network.

`Load more` requests another 20 date-ordered articles directly from SQLite. Once SQLite is exhausted, loading stops; RSS does not provide a standard way to request older pages that are no longer present in a publisher's feed **[TODO]**.

While the app remains open, it refreshes feeds every 15 minutes. The Reload button triggers the same synchronization manually.

## Archive lifecycle

Archiving an article removes it from the main feed and places it in the archive. An archived article can be:

- **Reinstated**, returning it to the main feed.
- **Deleted** immediately from the archive.
- Automatically deleted after seven days on the next launch or refresh.

Deleted article content is removed from SQLite. A small tombstone containing only its ID is retained for 90 days so an item still present in an RSS response is not immediately inserted again.

Unarchived articles that have not appeared in a feed response for 90 days are pruned. Articles still present in a feed have their `last_seen_at` value refreshed and remain cached.

## Source configuration

Manage feeds from **Settings** inside the app. Changes are written to:

```text
~/Library/Application Support/<app-bundle-identifier>/feeds.json
```

A fresh installation creates this file automatically with an empty feed list. Add, edit, or delete feeds in Settings; rebuilding the app is not required.

The configuration format is a JSON array. For example:

```json
[
  {
    "name": "Blog name",
    "rss": "https://example.com/feed.xml",
    "type": "blog"
  },
  {
    "name": "Channel name",
    "rss": "https://www.youtube.com/feeds/videos.xml?channel_id=<channel-ID>",
    "type": "youtube"
  }
]
```

YouTube channel URLs are resolved to their channel feed when saved.

## Storage

Application data is stored outside the repository. For example:

```text
~/Library/Application Support/com.<app-bundle-identifier>.<name>-feed/feeds.json
~/Library/Application Support/com.<app-bundle-identifier>.<name>-feed/archive.sqlite3
```

Despite its historical filename **[TODO]**, `archive.sqlite3` now contains the article cache, archive state, feed HTTP validators, and deletion tombstones. Existing databases are migrated in place when the app launches.

## Development

Install JavaScript dependencies once:

```sh
npm install
```

Run the development app:

```sh
npm run dev
```

The development command opens the native Tauri app, not a standalone browser version.

## Build and install

Build the release app:

```sh
npm run build
```

For example with Mac, the bundle is created at:

```text
src-tauri/target/release/bundle/macos/Mouser.app
```

After quitting Mouser, update the copy in Applications with:

```sh
ditto src-tauri/target/release/bundle/macos/Mouser.app /Applications/Mouser.app
```

Because the Dock item points to `/Applications/Mouser.app`, replacing the bundle at that path keeps the existing Dock pin.

## Architecture

- `templates/index.html`: bundled WebView interface
- `src-tauri/src/lib.rs`: feed synchronization, parsing, SQLite persistence, pagination, archive lifecycle, and native commands
- `src-tauri/tauri.conf.json`: macOS window and bundle configuration
- `src-tauri/capabilities/` and `src-tauri/permissions/`: restricted IPC access for the bundled interface

YouTube embeds receive the installed app's bundle identifier through the macOS WebView base URL. This allows inline playback in release builds without introducing a localhost server.
