# test-search.ps1 — Test the Setu CardDAV server with a phone number lookup.
#
# Usage:
#   .\test-search.ps1 <phone-number>
#   .\test-search.ps1 5551234567
#   .\test-search.ps1 "+1 (555) 123-4567"
#
# Optional environment variables:
#   SETU_PORT     — CardDAV server port (default: 5232)
#   SETU_PASSWORD — CardDAV password (default: read from `setu.exe --show-carddav-password`)

param(
    [Parameter(Mandatory=$true, Position=0)]
    [string]$PhoneNumber
)

$port = if ($env:SETU_PORT) { $env:SETU_PORT } else { "5232" }
$baseUrl = "http://localhost:$port"

# Get CardDAV password
if ($env:SETU_PASSWORD) {
    $password = $env:SETU_PASSWORD
} else {
    $output = & setu.exe --show-carddav-password 2>&1
    $match = $output | Select-String -Pattern 'Password:\s*(.+)$'
    if ($match) {
        $password = $match.Matches[0].Groups[1].Value.Trim()
    } else {
        Write-Host "ERROR: Could not read CardDAV password. Set SETU_PASSWORD or ensure setu.exe is in PATH." -ForegroundColor Red
        exit 1
    }
}

$cred = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("setu:$password"))
$headers = @{
    "Authorization" = "Basic $cred"
    "Content-Type"  = "application/xml; charset=utf-8"
    "Depth"         = "1"
}

# Step 1: Check server is reachable
Write-Host "Connecting to $baseUrl ..." -ForegroundColor Cyan
try {
    $null = Invoke-WebRequest -Uri "$baseUrl/.well-known/carddav" -MaximumRedirection 0 -ErrorAction Stop -Headers @{ "Authorization" = "Basic $cred" }
} catch {
    if ($_.Exception.Response.StatusCode.value__ -eq 301) {
        Write-Host "  Server is reachable." -ForegroundColor Green
    } else {
        Write-Host "  ERROR: Server not reachable — is Setu running?" -ForegroundColor Red
        Write-Host "  $($_.Exception.Message)" -ForegroundColor Red
        exit 1
    }
}

# Step 2: Search for phone number via REPORT
Write-Host "Searching for: $PhoneNumber" -ForegroundColor Cyan

$body = @"
<?xml version="1.0" encoding="UTF-8"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:getetag/>
    <C:address-data/>
  </D:prop>
  <C:filter>
    <C:prop-filter name="TEL">
      <C:text-match collation="i;unicode-casemap" match-type="contains">$PhoneNumber</C:text-match>
    </C:prop-filter>
  </C:filter>
</C:addressbook-query>
"@

try {
    $response = Invoke-WebRequest -Uri "$baseUrl/addressbook/" -Method "REPORT" -Headers $headers -Body $body -ContentType "application/xml; charset=utf-8"
} catch {
    Write-Host "ERROR: REPORT request failed." -ForegroundColor Red
    Write-Host "  Status: $($_.Exception.Response.StatusCode.value__)" -ForegroundColor Red
    Write-Host "  $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}

# Step 3: Parse results
$xml = [xml]$response.Content
$ns = New-Object Xml.XmlNamespaceManager($xml.NameTable)
$ns.AddNamespace("D", "DAV:")
$ns.AddNamespace("C", "urn:ietf:params:xml:ns:carddav")

$responses = $xml.SelectNodes("//D:response", $ns)

if ($responses.Count -eq 0) {
    Write-Host "  No contacts found for '$PhoneNumber'." -ForegroundColor Yellow
    exit 0
}

Write-Host "  Found $($responses.Count) contact(s):" -ForegroundColor Green
Write-Host ""

foreach ($r in $responses) {
    $href = $r.SelectSingleNode("D:href", $ns).'#text'
    $vcard = $r.SelectSingleNode(".//C:address-data", $ns).'#text'

    if ($vcard) {
        $fn = ($vcard -split "`n" | Where-Object { $_ -match "^FN:" }) -replace "^FN:", "" -replace "`r", ""
        $tels = ($vcard -split "`n" | Where-Object { $_ -match "^TEL" }) | ForEach-Object {
            ($_ -split ":")[-1].Trim("`r")
        }
        Write-Host "  Name:  $fn" -ForegroundColor White
        foreach ($tel in $tels) {
            Write-Host "  Phone: $tel" -ForegroundColor Gray
        }
        Write-Host "  Href:  $href" -ForegroundColor DarkGray
        Write-Host ""
    }
}
