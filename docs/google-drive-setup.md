# Connect Google Drive directly

Subscriptions Reminder uses Google browser authorization with PKCE and a temporary loopback callback. It asks only for `https://www.googleapis.com/auth/drive.appdata`, which grants access to its private application data instead of all Drive files. The EXE does not include a shared Google OAuth client, so a project owner must create a Desktop app client once.

## Create the OAuth client

1. Open [Google Cloud Console](https://console.cloud.google.com/) and create or select a project. A name such as **Subscriptions Reminder** is sufficient.
2. In **APIs & Services → Library**, find **Google Drive API** and enable it for that project.
3. Open **Google Auth Platform** or **OAuth consent screen**. Configure the app name, support email, and developer contact email. For a personal Google account, select an external audience. If the project is in Testing, add the Google accounts you will sign in with as test users.
4. Under **Data Access**, add `https://www.googleapis.com/auth/drive.appdata`. This app does not need the broad `drive` scope.
5. Under **Clients** or **APIs & Services → Credentials**, create an **OAuth client ID** and select **Desktop app** as the application type. Do not create a Web application or service account client.
6. Download its JSON configuration. Google commonly names it `client_secret_...apps.googleusercontent.com.json`.
7. In Subscriptions Reminder, open **Google Drive → Import Google OAuth JSON...** and select that file.
8. Click **Connect Google Drive**. Complete account selection and permission approval in your browser, then return to the application. Only you should enter Google credentials or approve the consent screen.

Google Cloud screen names can vary. The important requirements are an enabled Drive API, a correctly configured consent screen, the app-data scope, and a Desktop app client. See [Google's desktop OAuth guide](https://developers.google.com/identity/protocols/oauth2/native-app) and [Drive app-data guide](https://developers.google.com/workspace/drive/api/guides/appdata).

## Testing versus production

Google states that refresh tokens for external apps left in **Testing** expire after seven days when using scopes such as Drive app-data. The application preserves offline changes and asks you to reconnect when a session expires. For ongoing use, configure the project's publishing status appropriately and complete any branding or verification requirements Google presents. See [Google's refresh-token documentation](https://developers.google.com/identity/protocols/oauth2#expiration).

This repository does not create a Google Cloud project, enable APIs on your behalf, or automate sign-in or consent. No client configuration, access token, or refresh token should be committed to GitHub.

## More than one computer

Use the same Desktop client JSON and Google account on each computer. Sign in separately on each installation. Do not copy `settings.json` or replication checkpoint files between computers: protected credentials belong to the Windows account and each installation maintains its own device identity.

Data uses Google's hidden application-data space and is not displayed as an ordinary file in My Drive. Each installation writes a separate replication snapshot. Changes and deletions converge automatically; competing versions of the same subscription require an explicit choice in the application's conflict review.

## Troubleshooting

- **Connect is unavailable:** import a Desktop client JSON first.
- **Google rejects the client:** confirm the JSON uses the `installed` section and belongs to the intended project.
- **Access blocked or test-user error:** check the consent screen, audience, test users, and Google verification requirements. Do not bypass Google security warnings.
- **Drive API refused access:** enable Google Drive API in the same project and reconnect with app-data permission.
- **Sign-in times out:** complete it within three minutes and allow the system browser to return to `127.0.0.1` on the temporary local port. The application does not expose a public callback server.
- **Offline or quota error:** keep using the local copy; automatic sync retries with backoff, or use Sync now after the problem is resolved.
- **Conflicts:** review the stored versions and choose one, or keep all subscription versions as separate records. No version is silently discarded.
- **Large database:** synchronization currently limits each snapshot to 8 MB. Local data remains intact if this limit is reached.

Google and AI/Telegram credentials remain local and protected with Windows DPAPI. Subscription data is JSON protected by HTTPS and Google's storage controls; this version does not provide master-password encryption of the payload.
