# Greg's RSS feed
Self-hosted, personal feed aggregator, built as a lightweight Tauri desktop app.

Features a live button to trigger feed reload, search, persistent multi-theme switcher, the ability to parse and embed YouTube videos directly, a SQLite-backed archive, and a modal popup to display full articles or video descriptions complete with controls for adjusting font size.

I will be making updates to this project as my personal needs/use cases evolve.

## Desktop app

```sh
npm install
npm run dev
```

Build a Dock-pinnable macOS app with:

```sh
npm run build
```

The built app lands at `src-tauri/target/release/bundle/macos/Greg's Feed.app`.

## Storage

The app no longer depends on Python, Flask, Nix, or this project folder at runtime.
RSS fetching, feed parsing, YouTube handling, archive persistence, and config loading
all live in the Rust/Tauri app.

On first launch, the app writes editable feed config and archive storage into the
app data directory:

```text
~/Library/Application Support/com.gregorycotton.gregs-feed/feeds.json
~/Library/Application Support/com.gregorycotton.gregs-feed/archive.sqlite3
```

Only articles marked Done are stored in SQLite, keyed by a stable hash of their URL,
so feed reloads do not create new rows for every article.

## Adding feeds

For the installed app, edit:

```text
~/Library/Application Support/com.gregorycotton.gregs-feed/feeds.json
```

Then use Reload Feed in the app, or restart the app. No rebuild is needed for that
live config file.

The private bundled starter list lives at `src-tauri/resources/default-feeds.json`.
That file is gitignored. Change it and run `npm run build` only when you want a
newly built app bundle to include updated defaults for a fresh install.

<img src="https://raw.githubusercontent.com/gregorycotton/gregorycotton/refs/heads/main/ontology/images/rss-feed/rss-feed-home.jpg">
