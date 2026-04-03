# archivr

An open-source self-hosted archiving tool. Work in progress.

## Milestones

- [ ] Archiving
  - [x] Archiving media files from social media platforms
    - [x] YouTube Videos
    - [x] Twitter Videos
    - [x] Instagram
    - [x] Facebook
    - [x] TikTok
    - [x] Reddit
    - [x] Snapchat
    - [ ] YouTube Posts (postponed)
  - [x] Archiving local files
  - [x] Archiving Twitter Tweets & Threads
  - [ ] Archiving files from cloud storage services (Google Drive, Dropbox, OneDrive) and from URLs
    - [ ] URLs
    - [ ] Google Drive
    - [ ] Dropbox
    - [ ] OneDrive
    - (Some of these could be postponed for later.)
  - [ ] Archiving Twitter articles
  - [ ] Archive web pages (HTML, CSS, JS, images)
  - [ ] Archiving emails (???)
    - [ ] Gmail
    - [ ] Outlook
    - [ ] Yahoo Mail
- [ ] Management
  - [ ] Deduplication
  - [ ] Tagging system
  - [ ] Search functionality
  - [ ] Categorization
  - [ ] Metadata extraction and storage
- [ ] User Interface
  - [ ] Web-based UI
- [ ] Backup and Sync
  - [ ] Cloud backup (AWS S3, Google Cloud Storage)
  - [ ] Local backup

## Motivation

There are two driving factors behind this project:

- In the age of information, all data is ephemeral. Social media platforms frequently delete content, and cloud storage services can become inaccessible and unreliable. Being able to archive important data is _very important_ for preserving personal memories and digital history.
- I will be creating a small encyclopedia for my future family and kids. Therefore, I want to make sure that all the information I gather is preserved and accessible for future reference.

This project aims to provide a reliable solution for archiving important data from various sources, ensuring that users can preserve their digital assets for the long term.

## Archive Inputs

`archivr archive <path>` currently accepts three kinds of inputs:

- Local files via `file://...`
- Direct platform URLs
- Platform shorthand inputs such as `tweet:...`, `yt:...`, or `instagram:...`

### Supported Platforms

- Local files: `file:///absolute/path/to/file.ext`
- YouTube media: standard video/short URLs, plus [shorthand video inputs](#supported-shorthand-inputs)
- X/Twitter media from Tweets: normal Tweet URLs or the `tweet:media:ID` shorthand
- X/Twitter Tweet content scrape: [Tweet and Thread shorthands](#supported-shorthand-inputs). (These are saved as TOML files in `raw_tweets/`)
- Instagram, Facebook, TikTok, Reddit, Snapchat: direct URLs or platform-prefixed shorthand passed through to `yt-dlp`

### Supported Shorthand Inputs

- YouTube video/short media:
  - `yt:video/ID`
  - `youtube:video/ID`
  - `yt:short/ID`
  - `yt:shorts/ID`
  - `youtube:shorts/ID`
- X/Twitter tweet TOML content:
  - `tweet:ID`
  - `x:tweet:ID`
  - `x:x:ID`
  - `twitter:x:ID`
  - `twitter:tweet:ID`
- X/Twitter media/video download:
  - `tweet:media:ID`
- X/Twitter thread TOML content:
  - `x:thread:ID`
  - `twitter:thread:ID`
- Other platform shorthands:
  - `instagram:ID`
  - `facebook:ID`
  - `tiktok:ID`
  - `reddit:ID`
  - `snapchat:ID`

### Environment Variables

- `ARCHIVR_YT_DLP`
  - Optional.
  - Overrides the `yt-dlp` binary used for YouTube, X media posts, Instagram, Facebook, TikTok, Reddit, and Snapchat downloads.
- `ARCHIVR_TWITTER_CREDENTIALS_FILE`
  - Required for tweet/thread scraping inputs such as `tweet:ID` and `x:thread:ID`.
  - Must point to a cookies file for the vendored scraper.
- `ARCHIVR_TWEET_SCRAPER`
  - Optional.
  - Overrides the tweet scraper script path. Default: `vendor/twitter/scrape_user_tweet_contents.py`.
- `ARCHIVR_TWEET_PYTHON`
  - Optional.
  - Overrides the Python executable used to run the tweet scraper. Default: `python3`.

### Current Limitations

- Arbitrary `http://` or `https://` pages are not archived yet unless they match one of the currently supported platforms above.
- Local files currently need to be passed as `file://...` paths.

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE.md) file for details.
