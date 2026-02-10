# Setu

**CardDAV bridge for Google Contacts.**

Setu syncs your Google Contacts and serves them over CardDAV, so any CardDAV-compatible client. I built this for OpenBubbles since I could not get google contacts to work.

- **Windows**: system tray with MSI installer (auto-start on login)
- **Linux**: runs as a systemd user service with AppImage distribution
- Syncs incrementally via the Google People API
- Serves contacts as vCard 3.0 over a local CardDAV server
- On-demand phone number search — queries Google in real time for numbers not in the local cache
- All data encrypted at rest with SQLCipher (AES-256)
- Credentials stored in the OS keyring (falls back to file-based vault if no keyring service is available)

## Installation

### Windows — MSI (recommended)

Download the latest `setu-<version>.msi` from [Releases](https://github.com/cybercypher/setu/releases) and double-click to install. The installer:

- Installs `setu.exe` to `%LOCALAPPDATA%\Setu\`
- Registers auto-start on login (HKCU Run key)
- Launches Setu immediately after install
- On uninstall, cleans up all data and credentials

### Linux — AppImage

Download the latest `Setu-<version>-x86_64.AppImage` from [Releases](https://github.com/cybercypher/setu/releases), then:

```bash
chmod +x Setu-*-x86_64.AppImage
./Setu-*-x86_64.AppImage
```

The AppImage bundles all dependencies (GTK, D-Bus, etc.) — no system packages required. On first run the settings window opens for Google Cloud credentials. After setup, Setu automatically installs itself as a **systemd user service** that runs in the background and auto-starts on login.

```bash
# Service management
systemctl --user status setu      # check status
systemctl --user restart setu     # restart
journalctl --user -u setu -f      # view logs

# Manual install/uninstall
./Setu-*-x86_64.AppImage --install
./Setu-*-x86_64.AppImage --uninstall
```

### From source

See [Building from source](#building-from-source) below.

## Google Cloud Setup

Setu requires a Google Cloud project with the People API enabled and OAuth 2.0 Desktop credentials. This is a one-time setup.

### 1. Create a Google Cloud project

1. Open [Google Cloud Console — Create Project](https://console.cloud.google.com/projectcreate)
2. Enter a project name (e.g. `Setu Contacts`)
3. Click **Create**

### 2. Enable the People API

1. Open [Enable People API](https://console.cloud.google.com/flows/enableapi?apiid=people.googleapis.com)
2. Make sure your project is selected in the top dropdown
3. Click **Enable**

Alternatively, go to [API Library](https://console.cloud.google.com/apis/library), search for "People API", and click **Enable**.

### 3. Configure the OAuth consent screen

#### 3a. Branding

1. Open [Google Auth Platform — Branding](https://console.cloud.google.com/auth/branding)
2. If prompted, click **Get Started**
3. Set **App name** to `Setu`
4. Select your email under **User support email**
5. Click **Next**

#### 3b. Audience

6. Select **External** (required for personal Gmail accounts)
7. Click **Next**

#### 3c. Contact information

8. Enter your email address
9. Click **Next**

#### 3d. Finish

10. Check **I agree to the Google API Services: User Data Policy**
11. Click **Continue**, then **Create**

#### 3e. Add yourself as a test user

12. Open [Google Auth Platform — Audience](https://console.cloud.google.com/auth/audience)
13. Under **Test users**, click **Add users**
14. Enter your Gmail address and click **Save**

> **Testing vs Production mode:** Your app starts in "Testing" mode. In Testing mode, only users explicitly added as test users can authorize the app, and OAuth tokens expire every 7 days (you'll need to re-login weekly). To avoid token expiry, publish the app to Production mode under [Google Auth Platform — Audience](https://console.cloud.google.com/auth/audience). No verification is required. Production mode is recommended.

### 4. Create OAuth 2.0 credentials

1. Open [Google Auth Platform — Clients](https://console.cloud.google.com/auth/clients)
2. Click **Create Client**
3. Set **Application type** to **Desktop app**
4. Name it (e.g. `Setu Desktop`)
5. Click **Create**

A dialog appears with your **Client ID** and **Client Secret**. Copy both values — you'll paste them into Setu's settings window.

> You can also click the download icon to save a `client_secret_*.json` file as a backup. The secret may not be visible again after you close this dialog.

## First Run

1. Launch `setu.exe` (or let the MSI installer launch it)
2. The settings window opens automatically on first run
3. Paste your **Client ID** and **Client Secret** into the corresponding fields
4. Click **Save Settings**, then **Login with Google**
5. Complete the sign-in flow in your browser
6. Close the settings window — Setu starts syncing in the background

Setu appears in the system tray. Right-click the tray icon to:

- **Settings** — reopen the settings window
- **Sync Now** — trigger an immediate sync
- **Quit Setu** — shut down

## Connecting a CardDAV Client

Setu runs a CardDAV server on `localhost` (default port `5232`). Configure your client with:

| Setting | Value |
|---|---|
| Server URL | `http://localhost:5232` (or `https://` if TLS is enabled) |
| Username | anything (e.g. `setu`) |
| Password | shown in the settings window |

To view the password from the command line:

```
setu.exe --show-carddav-password
```

### Tested clients

- **OpenBubbles** — CardDAV contact sync (Google Contacts native integration could not be used because the app is blocked by Google)

## HTTPS / TLS (Optional)

> **Note:** TLS support is currently untested.

Setu can encrypt localhost traffic using a local Certificate Authority. This protects against localhost traffic sniffing.

1. Open **Settings** and check **Enable HTTPS**
2. Click **Save Settings** — Setu generates a local CA and server certificate
3. Windows shows a one-time security dialog to trust the CA (added to CurrentUser store, no admin needed)
4. Restart Setu — the CardDAV server now listens on `https://localhost:5232`
5. Update your CardDAV client URL from `http://` to `https://`

All Windows apps (Outlook, CalDav Synchronizer, browsers) trust the certificate automatically after step 3. The CA and server certificates are stored in `%APPDATA%\setu\`.

## CLI Flags

| Flag | Description |
|---|---|
| *(none)* | Start with system tray (default) |
| `--settings` | Open the settings GUI and exit |
| `--headless` | Run without the tray (server + sync only) |
| `--show-carddav-password` | Print the CardDAV Basic Auth credentials and exit |
| `--install` | Install systemd user service (Linux only) |
| `--uninstall` | Remove systemd user service (Linux only) |

## Configuration

Config file: `%APPDATA%\setu\config.json`

| Key | Default | Description |
|---|---|---|
| `google_client_id` | *(empty)* | OAuth Client ID |
| `sync_interval_secs` | `900` | Sync interval in seconds (15 min) |
| `server_port` | `5232` | CardDAV server port |
| `use_tls` | `false` | Enable HTTPS for the CardDAV server |

The client secret is stored in the OS keyring, not in the config file.

## Data Files

**Windows** (`%APPDATA%\setu\`):

| Path | Description |
|---|---|
| `config.json` | Configuration |
| `setu.db` | Encrypted contact database (SQLCipher) |
| `oauth_token.json` | Cached OAuth token |
| `setu.log` | Runtime logs |
| `ca.crt` / `ca.key` | Local CA (created when HTTPS is enabled) |
| `server.crt` / `server.key` | Server certificate signed by local CA |

**Linux** (`~/.local/share/setu/`):

| Path | Description |
|---|---|
| `config.json` | Configuration |
| `setu.db` | Encrypted contact database (SQLCipher) |
| `vault.json` | File-based vault (only when OS keyring is unavailable) |
| `oauth_token.json` | Cached OAuth token |
| `setu.log` | Runtime logs |

## Security

- **SQLCipher** — AES-256 full-database encryption with `PRAGMA secure_delete = ON`
- **OS Keyring** — DB encryption key, OAuth token, CardDAV password, and Google client secret are stored in the OS keyring (Windows Credential Manager or Linux Secret Service)
- **File-based vault fallback** — if no keyring service is available (e.g. no gnome-keyring), secrets are stored in `~/.local/share/setu/vault.json` with `chmod 600` permissions
- **CardDAV Basic Auth** — password is auto-generated (24 alphanumeric characters) and stored securely
- **Local only** — the CardDAV server binds to `127.0.0.1`, never exposed to the network

## Building from Source

### Prerequisites

Setu cross-compiles from WSL2 (Linux) to Windows. Run the setup script to install toolchains:

```bash
./setup_wsl.sh
```

This installs:
- `gcc-mingw-w64-x86-64` and related cross-compilation tools
- Rust (via rustup) with the `x86_64-pc-windows-gnu` target
- `msitools` (provides `wixl` for MSI generation)

### Build

```bash
# Release binary
cargo build --release --target x86_64-pc-windows-gnu

# MSI installer
./build_local_msi.sh

# MSI with self-signed code signature
./build_local_msi.sh --sign
```

The release binary is at `target/x86_64-pc-windows-gnu/release/setu.exe` (~8 MB, stripped + LTO).

### Linux AppImage

```bash
./build_appimage.sh
```

This builds a native Linux release binary and packages it as `Setu-x86_64.AppImage` using `linuxdeploy` with the GTK plugin. All shared library dependencies are bundled automatically. The script downloads `linuxdeploy` on first run.

Build prerequisites: Rust toolchain, `libgtk-3-dev`, `libdbus-1-dev`, `libxdo-dev`, `pkg-config`.

### Run Tests

```bash
cargo test --target x86_64-unknown-linux-gnu --lib
```

44 unit tests across db (15), vcard (14), and server (16).

## License

This project is licensed under the [GNU Affero General Public License v3.0](LICENSE).

For commercial licensing (use without AGPL obligations), [contact us](https://forms.gle/75Lf2BNyeaRnvAWf9).
