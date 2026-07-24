# v0.2.0 verification

Verified on Windows with Rust 1.96.1:

- Release compilation with a statically linked MSVC runtime; the packaged executable imports only Windows system DLLs.
- Unit tests for reminder lead times, overdue catch-up, paused subscriptions, month-end and leap-year renewal, and daily per-channel deduplication.
- Storage tests for save/reopen, backups, invalid JSON preservation, external change rejection, missing synced data, and switching data folders.
- Stable release asset selection, semantic version comparison, official download URL checks, and checksum parsing.
- Windows DPAPI token encryption/decryption and rejection of invalid protected data.
- Compatible Chat Completions HTTP requests with bearer authentication and inline images, model listing, and redirect blocking.
- Parsing of multiple detected records, original-language names, incomplete model responses, and missing expiry dates.
- Atomic import validation, persistence, and exact-duplicate skipping.
- Formatting and Clippy checks.
- An interactive launch of the packaged application with isolated sample data, dashboard rendering, opening Settings, and exiting through Quit.
- A live request from the packaged application's image importer to an authenticated AI-Hub Universal endpoint. All three demonstration subscription names, expiry dates, prices, and currencies were recognized correctly. The response also identified the overdue subscription.
- Real application frame captures of the current dashboard, Settings, lower settings, and image import review, without operating-system cursor overlays.

All 16 automated tests pass. OpenRouter was checked against its official image-input protocol and the same client was exercised against local HTTP fixtures; a paid OpenRouter call was not made. The following remain unverified and should be checked manually:

- Closing to the tray and reopening by single click, double click, and the tray Open menu.
- Restoring a minimized window; reopening a background-started instance; duplicate-launch activation; repeated hide/reopen cycles; and Quit while hidden.
- Visible Windows notification delivery and continuing reminders while hidden.
- Real Telegram delivery with user-supplied bot credentials.
- Actual Google Drive synchronization across computers.
- Downloading, replacing the installed executable, and restarting through the updater. Version/asset/checksum logic is covered by tests; the full installation flow is not yet verified against a newer published release.

No Telegram credentials or real subscription data were used during verification. The private AI-Hub credential was used with explicit user authorization, kept outside the repository, and stored with Windows DPAPI encryption. README images contain demonstration data only.
