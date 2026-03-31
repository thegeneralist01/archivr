# archivr

An open-source self-hosted archiving tool. Work in progress.

## Milestones
- [ ] Archiving
    - [X] Archiving media files from social media platforms
        - [X] YouTube Videos
        - [X] Twitter Videos
        - [X] Instagram
        - [X] Facebook
        - [X] TikTok
        - [X] Reddit
        - [X] Snapchat
        - [ ] YouTube Posts (postponed)
    - [X] Archiving local files
    - [ ] Archiving files from cloud storage services (Google Drive, Dropbox, OneDrive) and from URLs
        - [ ] URLs
        - [ ] Google Drive
        - [ ] Dropbox
        - [ ] OneDrive
        - (Some of these could be postponed for later.)
    - [X] Archiving Twitter threads
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
- In the age of information, all data is ephemeral. Social media platforms frequently delete content, and cloud storage services can become inaccessible and unreliable. Being able to archive important data is *very important* for preserving personal memories and digital history.
- I will be creating a small encyclopedia for my future family and kids. Therefore, I want to make sure that all the information I gather is preserved and accessible for future reference.

This project aims to provide a reliable solution for archiving important data from various sources, ensuring that users can preserve their digital assets for the long term.

## Twitter/X Archive Inputs
- Tweet content TOML: `tweet:ID`, `x:tweet:ID`, `x:x:ID`, `twitter:x:ID`, `twitter:tweet:ID`
- Tweet media/video: `tweet:media:ID`
- Thread TOML content: `x:thread:ID`, `twitter:thread:ID`

Twitter tweet/thread scraping requires `ARCHIVR_TWITTER_CREDENTIALS_FILE` to point to a cookies file for the vendored scraper.

## License
This project is licensed under the MIT License. See the [LICENSE](LICENSE.md) file for details.
