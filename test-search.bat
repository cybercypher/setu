@echo off
if "%~1"=="" (
    echo Usage: test-search.bat ^<phone-number^> ^<password^>
    echo Example: test-search.bat 5551234567 abc123
    exit /b 1
)
if "%~2"=="" (
    echo Usage: test-search.bat ^<phone-number^> ^<password^>
    echo Example: test-search.bat 5551234567 abc123
    exit /b 1
)
curl.exe -s -u "setu:%~2" -X REPORT http://localhost:5232/addressbook/ -H "Content-Type: application/xml" -H "Depth: 1" -d "<?xml version='1.0'?><C:addressbook-query xmlns:D='DAV:' xmlns:C='urn:ietf:params:xml:ns:carddav'><D:prop><D:getetag/><C:address-data/></D:prop><C:filter><C:prop-filter name='TEL'><C:text-match collation='i;unicode-casemap' match-type='contains'>%~1</C:text-match></C:prop-filter></C:filter></C:addressbook-query>"
