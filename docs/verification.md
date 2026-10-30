# v0.1.0 verification

Verified on Windows with Rust 1.96.1:

- Release compilation with a statically linked MSVC runtime; the packaged executable imports only Windows system DLLs.
- Unit tests for reminder lead times, overdue catch-up, paused subscriptions, month-end and leap-year renewal, and daily per-channel deduplication.
- Storage tests for save/reopen, backups, invalid JSON preservation, external change rejection, missing synced data, and switching data folders.
- Stable release asset selection, semantic version comparison, official download URL checks, and checksum parsing.
- Windows DPAPI token encryption/decryption and rejection of invalid protected data.
- Formatting and Clippy checks.
- An interactive launch of the packaged application with isolated sample data, dashboard rendering, opening Settings, and exiting through Quit.

Interactive computer control was stopped by the user with Escape. The final typography changes were made after the captured preview. The following remain unverified and should be checked manually:

- Refreshing dashboard, full Settings, and storage/update screenshots.
- Closing to the tray and reopening by single click, double click, and the tray Open menu.
- Restoring a minimized window; reopening a background-started instance; duplicate-launch activation; repeated hide/reopen cycles; and Quit while hidden.
- Visible Windows notification delivery and continuing reminders while hidden.
- Real Telegram delivery with user-supplied bot credentials.
- Actual Google Drive synchronization across computers.
- Downloading, replacing the installed executable, and restarting through the updater. Version/asset/checksum logic is covered by tests; the full installation flow is not yet verified against a newer published release.

No Telegram credentials or real subscription data were used during verification.
