# Connect Google Drive directly

Subscriptions Reminder uses Google browser authorization with PKCE and a temporary loopback callback. It asks only for `https://www.googleapis.com/auth/drive.appdata`, which grants access to its private application data instead of all Drive files. The EXE includes the application's Desktop OAuth client. End users click **Google Drive → Connect Google Drive** and complete Google sign-in in their browser; no JSON import or Cloud project is required.

The bundled project is currently in **Testing**. Only accounts registered as test users can sign in, and Google may expire their refresh tokens after seven days. The maintainer must complete the project's production configuration and any required Google verification before advertising unrestricted sign-in. A built-in client alone does not remove Google's audience restrictions.

## Maintainer configuration

OAuth configuration is excluded from Git. For a local configured build, save the downloaded Desktop client JSON as `src/drive/google-oauth.json`; this exact path and Google's `client_secret_*.json` filenames are ignored. Alternatively, provide the JSON through the `SUBSCRIPTIONS_GOOGLE_OAUTH_JSON` build environment variable. The build script writes it only into Cargo's ignored build directory and includes it in the executable. A checkout without either source still builds with optional custom-client import.

For GitHub Releases, the repository owner must create an Actions repository secret named **`GOOGLE_DESKTOP_OAUTH_JSON`** containing the full downloaded Desktop client JSON. Open **Settings → Secrets and variables → Actions → New repository secret**. The workflow supplies it only to the release build on the trusted main branch, never to pull-request builds. Release builds fail with a configuration message if the secret is missing instead of publishing an executable with unavailable Google sign-in. The source file and its contents must not be committed. See [GitHub's secrets documentation](https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/use-secrets).

Keeping this JSON out of Git and build logs does **not** make it confidential inside a distributed executable. Desktop applications are public OAuth clients; their embedded `client_id` and `client_secret` can be extracted. Google sign-in, user consent, and PKCE protect access to account data. A client configuration alone does not grant access to a user's Drive. Never embed account access tokens, refresh tokens, service-account private keys, or unrelated API keys. User tokens stay local and are encrypted with Windows DPAPI. See [Google's Desktop OAuth documentation](https://developers.google.com/identity/protocols/oauth2/native-app).

Existing sessions retain the client they originally authorized, including after an application update. A custom-client user must disconnect before switching to the built-in connection. Each client has its own Drive app-data space, so changing clients does not migrate cloud data; reconnecting merges the retained local copy into the selected space.

## Optional custom client

1. Open [Google Cloud Console](https://console.cloud.google.com/) and create or select a project. A name such as **Subscriptions Reminder** is sufficient.
2. In **APIs & Services → Library**, find **Google Drive API** and enable it for that project.
3. Open **Google Auth Platform** or **OAuth consent screen**. Configure the app name, support email, and developer contact email. For a personal Google account, select an external audience. If the project is in Testing, add the Google accounts you will sign in with as test users.
4. Under **Data Access**, add `https://www.googleapis.com/auth/drive.appdata`. This app does not need the broad `drive` scope.
5. Under **Clients** or **APIs & Services → Credentials**, create an **OAuth client ID** and select **Desktop app** as the application type. Do not create a Web application or service account client.
6. Download its JSON configuration. Google commonly names it `client_secret_...apps.googleusercontent.com.json`.
7. In Subscriptions Reminder, expand **Google Drive → Advanced: custom Google connection**, click **Import Google OAuth JSON...**, and select that file.
8. Click **Connect Google Drive**. Complete account selection and permission approval in your browser, then return to the application. Only you should enter Google credentials or approve the consent screen.

Google Cloud screen names can vary. The important requirements are an enabled Drive API, a correctly configured consent screen, the app-data scope, and a Desktop app client. See [Google's desktop OAuth guide](https://developers.google.com/identity/protocols/oauth2/native-app) and [Drive app-data guide](https://developers.google.com/workspace/drive/api/guides/appdata).

## Testing versus production

Google states that refresh tokens for external apps left in **Testing** expire after seven days when using scopes such as Drive app-data. The application preserves offline changes and asks you to reconnect when a session expires. For ongoing use, configure the project's publishing status appropriately and complete any branding or verification requirements Google presents. See [Google's refresh-token documentation](https://developers.google.com/identity/protocols/oauth2#expiration).

Google sign-in and consent are completed by the account owner in the system browser. Never commit OAuth configuration, access tokens, or refresh tokens to GitHub. If a file was previously tracked, adding an ignore rule alone does not untrack it or remove it from published history.

## More than one computer

Use the same application Google connection and Google account on each computer. With a custom client, import the same Desktop JSON on each device. Sign in separately on each installation. Do not copy `settings.json` or replication checkpoint files between computers: protected credentials belong to the Windows account and each installation maintains its own device identity.

Data uses Google's hidden application-data space and is not displayed as an ordinary file in My Drive. Each installation writes a separate replication snapshot. Changes and deletions converge automatically; competing versions of the same subscription require an explicit choice in the application's conflict review.

## Troubleshooting

- **Connect is unavailable:** the build lacks a configured bundled client. Use an official configured release, or import your own client under Advanced.
- **Google rejects the client:** confirm the JSON uses the `installed` section and belongs to the intended project.
- **Access blocked or test-user error:** check the consent screen, audience, test users, and Google verification requirements. Do not bypass Google security warnings.
- **Drive API refused access:** enable Google Drive API in the same project and reconnect with app-data permission.
- **Sign-in times out:** complete it within three minutes and allow the system browser to return to `127.0.0.1` on the temporary local port. The application does not expose a public callback server.
- **Offline or quota error:** keep using the local copy; automatic sync retries with backoff, or use Sync now after the problem is resolved.
- **Conflicts:** review the stored versions and choose one, or keep all subscription versions as separate records. No version is silently discarded.
- **Large database:** synchronization currently limits each snapshot to 8 MB. Local data remains intact if this limit is reached.

Google and AI/Telegram credentials remain local and protected with Windows DPAPI. Subscription data is JSON protected by HTTPS and Google's storage controls; this version does not provide master-password encryption of the payload.
