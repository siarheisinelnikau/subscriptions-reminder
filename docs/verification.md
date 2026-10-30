# v0.3.0 verification

Verified on Windows with Rust 1.96.1:

- Release compilation with a statically linked MSVC runtime; the packaged executable imports only Windows system DLLs.
- Unit tests for reminder lead times, overdue catch-up, paused subscriptions, month-end and leap-year renewal, and daily per-channel deduplication.
- Storage tests for save/reopen, backups, invalid JSON preservation, external change rejection, missing synced data, and switching data folders.
- Stable release asset selection, semantic version comparison, official download URL checks, and checksum parsing.
- Windows DPAPI token encryption/decryption and rejection of invalid protected data.
- Compatible Chat Completions HTTP requests with bearer authentication and inline images, model listing, and redirect blocking.
- Parsing of multiple detected records, original-language names, incomplete model responses, and missing expiry dates.
- Atomic import validation, persistence, and exact-duplicate skipping.
- Google OAuth PKCE against the RFC test vector, callback state and duplicate-parameter rejection, split HTTP callback headers on a nonblocking Windows listener, Desktop client configuration validation, token refresh/rotation, permission checks, and revoked-session handling using local HTTP fixtures.
- Google Drive app-data request scoping, pagination, download validation, deterministic upload IDs, multipart create/media update requests, and expired-access-token handling using local HTTP fixtures.
- Two-device replication simulations for independent offline edits, concurrent conflicts and resolution, deletions, lost upload responses, and edits made while a download is in flight.
- Formatting and Clippy checks.
- An interactive launch of the packaged application with isolated sample data, dashboard rendering, opening Settings, and exiting through Quit.
- In v0.2.0, a live request from the packaged image importer to an authenticated AI-Hub Universal endpoint correctly recognized three demonstration subscriptions with their dates, prices, and currencies. The response also identified the overdue subscription.
- Real application frame captures of the dashboard, Settings, lower settings, Google Drive setup, and image import view, without operating-system cursor overlays.

All 35 automated tests pass. OpenRouter uses the documented image-input protocol and the client was exercised against local HTTP fixtures; a paid OpenRouter call was not made. Google requests were tested against HTTP fixtures and a multi-device simulator, **not a real Google account**. The following remain unverified and should be checked manually:

- Closing to the tray and reopening by single click, double click, and the tray Open menu.
- Restoring a minimized window; reopening a background-started instance; duplicate-launch activation; repeated hide/reopen cycles; and Quit while hidden.
- Visible Windows notification delivery and continuing reminders while hidden.
- Real Telegram delivery with user-supplied bot credentials.
- Actual Google browser sign-in, consent, session refresh, and Drive synchronization across computers. A Google Cloud project/Desktop OAuth client still needs to be created and imported by the account owner.
- Downloading, replacing the installed executable, and restarting through the updater. Version/asset/checksum logic is covered by tests; the full installation flow is not yet verified against a newer published release.

No Telegram credentials or real subscription data were used during verification. The private AI-Hub credential was used with explicit user authorization, kept outside the repository, and stored with Windows DPAPI encryption. README images contain demonstration data only.
