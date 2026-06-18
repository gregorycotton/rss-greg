# Greg's RSS feed
Self-hosted, personal feed aggregator, built as a Python web app with flask.

Features a live button to trigger feed reload, a fuzzy search, persistent multi-theme switcher, the ability to parse and embed YouTube videos directly, a local storage-backed archive, a modal popup to display full articles or video descriptions complete with controls for adjusting font size.

I will be making updates to this project as my personal needs/use cases evolve.

## Local setup

```sh
nix-shell
python app.py
```

Feed sources live in a local `.env` file so they do not get pushed to GitHub.
Use `.env.example` for the expected `RSS_SITE_...` format.

Archived article state lives in `archive.sqlite3`. Only articles marked Done are
stored, keyed by a stable hash of their URL, so feed reloads do not create new
rows for every article.

## Desktop app

```sh
nix-shell
npm install
npm run dev
```

Build a Dock-pinnable macOS app with:

```sh
npm run build
```

The built app lands at `src-tauri/target/release/bundle/macos/Greg's Feed.app`.
It starts the local Flask backend through Nix and uses this project folder as the backend root.
Backend logs are written to your system temp directory as `gregs-feed-backend.log`.

<img src="https://raw.githubusercontent.com/gregorycotton/gregorycotton/refs/heads/main/ontology/images/rss-feed/rss-feed-home.jpg">
