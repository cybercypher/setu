# Setu — WSL2 to Windows Execution & Testing Guide

## 1. Build

### One-time setup (if not done already)
```bash
chmod +x setup_wsl.sh && ./setup_wsl.sh
```

### Build the Windows .exe from WSL2
```bash
# Debug build (faster, includes debug symbols)
cargo build

# Release build (optimised, stripped, 8 MB)
cargo build --release
```

The binary is at:
```
target/x86_64-pc-windows-gnu/debug/setu.exe      # debug
target/x86_64-pc-windows-gnu/release/setu.exe     # release
```

### Run unit tests (on the Linux host)
```bash
cargo test --target x86_64-unknown-linux-gnu --lib
```

---

## 2. Running on Windows

### Option A: Launch from WSL2 terminal
```bash
# The .exe is a native Windows binary — WSL2 can execute it directly:
./target/x86_64-pc-windows-gnu/release/setu.exe
```
This will:
- Start the CardDAV server on `127.0.0.1:5232`
- Place a teal Setu icon in the Windows system tray
- Begin syncing Google Contacts (if credentials are configured)

### Option B: Copy to a Windows folder and double-click
```bash
cp target/x86_64-pc-windows-gnu/release/setu.exe /mnt/c/Users/$USER/Desktop/
```
Then double-click `setu.exe` on the desktop.

### Settings GUI
```bash
./target/x86_64-pc-windows-gnu/release/setu.exe --settings
```
Or right-click the tray icon → **Settings...**

### Configure Google OAuth (first time)
1. Go to https://console.cloud.google.com/apis/credentials
2. Create an **OAuth 2.0 Client ID** (type: Desktop application)
3. Enable the **People API** at https://console.cloud.google.com/apis/library/people.googleapis.com
4. Open Setu Settings (`--settings`), paste the Client ID and Secret, click Save
5. Restart Setu — it will open your browser for Google sign-in

---

## 3. Verifying the CardDAV Server

### From WSL2 (curl)

**Check the server is alive:**
```bash
curl -s -X OPTIONS http://localhost:5232/ -D -
```
Expected: `HTTP/1.1 200 OK` with `DAV: 1, 3, addressbook` header.

**Service discovery (PROPFIND on root):**
```bash
curl -s -X PROPFIND http://localhost:5232/ \
  -H "Depth: 0" \
  -H "Content-Type: application/xml" \
  -d '<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'
```
Expected: 207 Multi-Status XML with `<D:current-user-principal>` pointing to `/principals/`.

**List address book contents (PROPFIND Depth:1):**
```bash
curl -s -X PROPFIND http://localhost:5232/addressbook/ \
  -H "Depth: 1" \
  -H "Content-Type: application/xml" \
  -d '<?xml version="1.0"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>'
```
Expected: 207 Multi-Status XML with one `<D:response>` for the collection + one per contact.

**Fetch all contacts (REPORT):**
```bash
curl -s -X REPORT http://localhost:5232/addressbook/ \
  -H "Content-Type: application/xml" \
  -d '<?xml version="1.0"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:getetag/>
    <C:address-data/>
  </D:prop>
</C:addressbook-query>'
```
Expected: 207 Multi-Status with `<C:address-data>` containing vCard 3.0 text for each contact.

**Fetch a single vCard (GET):**
```bash
# Replace people_c1234567890 with an actual resource ID from the PROPFIND listing
curl -s http://localhost:5232/addressbook/people_c1234567890.vcf
```
Expected: Raw vCard 3.0 text beginning with `BEGIN:VCARD`.

---

## 4. Client Verification

### Mozilla Thunderbird
1. Open Thunderbird → **Address Books** → **New Address Book** → **CardDAV**
2. Enter: `http://localhost:5232/`
3. Username/password: leave blank (no auth on localhost)
4. Thunderbird will auto-discover the address book at `/addressbook/`
5. Contacts should appear after the initial sync

### Outlook + CalDav Synchronizer
1. Install [CalDav Synchronizer](https://caldavsynchronizer.org/) for Outlook
2. Add a new sync profile → **Type**: CardDAV
3. **DAV URL**: `http://localhost:5232/addressbook/`
4. Leave credentials empty or use any dummy value
5. Click **Test Settings** → should show a green checkmark
6. Sync to pull contacts into Outlook

### GNOME Contacts / Evolution (Linux — via WSL2 network)
1. Add an online account → **CardDAV**
2. URL: `http://localhost:5232/`
3. Contacts will appear after sync

---

## 5. Log Files

All runtime logs are written to:
```
%APPDATA%\setu\setu.log
```

From WSL2, this is typically:
```bash
cat /mnt/c/Users/$USER/AppData/Roaming/setu/setu.log
```

Set `RUST_LOG=setu=debug` for verbose output:
```bash
RUST_LOG=setu=debug ./target/x86_64-pc-windows-gnu/release/setu.exe
```

---

## 6. Data Files

| File | Location (Windows) | Purpose |
|------|-------------------|---------|
| `config.json` | `%APPDATA%\setu\config.json` | OAuth creds, sync interval, port |
| `setu.db` | `%APPDATA%\setu\setu.db` | SQLite contact cache |
| `oauth_token.json` | `%APPDATA%\setu\oauth_token.json` | Cached Google OAuth token |
| `setu.log` | `%APPDATA%\setu\setu.log` | Runtime log |
