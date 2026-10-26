# cimon upload - sends files or one-line values from this PC straight to the cimon server
# for one job, and prints the handles to pass to build/test/run as `uploads`. The bytes
# go from this PC to the server only - never through the Claude conversation.
# Served by the cimon server itself; the address below is that server's. The connect
# script keeps a copy in %USERPROFILE%\.cimon\upload.ps1.
#   powershell -NoProfile -ExecutionPolicy Bypass -File $HOME\.cimon\upload.ps1 <name>=<source> ...
# <source>: a file path | env:<VARIABLE> (its value) | prompt (asks, without echo)
#   e.g. keystore=C:\keys\release.jks keystore-password=env:EB_KEYSTORE_PASSWORD key-password=prompt
$ErrorActionPreference = 'Stop'
$CimonUrl = '{{URL}}'

function Fail([string]$m) { [Console]::Error.WriteLine("cimon upload: $m"); exit 1 }

if ($args.Count -eq 0) { Fail 'usage: upload.ps1 <name>=<file> | <name>=env:<VARIABLE> | <name>=prompt ...' }
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# the token through the same header helper Claude Code uses
$helper = Join-Path $env:USERPROFILE '.cimon\header.ps1'
if (-not (Test-Path $helper)) { Fail "no $helper - connect this PC first: irm $CimonUrl/connect.ps1 | iex" }
$auth = (& "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File $helper | Out-String | ConvertFrom-Json).Authorization
if (-not $auth) { Fail "$helper printed no Authorization header" }

$handles = [ordered]@{}
foreach ($item in $args) {
    $s = [string]$item
    $i = $s.IndexOf('=')
    if ($i -lt 1) { Fail "$s - expected <name>=<source>" }
    $name = $s.Substring(0, $i)
    $src = $s.Substring($i + 1)
    if ($name -cnotmatch '^[a-z0-9][a-z0-9_-]{0,63}$') { Fail "$name is not an upload name ([a-z0-9][a-z0-9_-]*, as the recipe writes it)" }
    if ($src -eq 'prompt') {
        $sec = Read-Host -AsSecureString $name
        $value = [System.Net.NetworkCredential]::new('', $sec).Password
        if ([string]::IsNullOrEmpty($value)) { Fail "$name - nothing was typed" }
        $kind = 'text'
        $body = [Text.Encoding]::UTF8.GetBytes($value)
    } elseif ($src.StartsWith('env:')) {
        $var = $src.Substring(4)
        $value = [Environment]::GetEnvironmentVariable($var)
        if ([string]::IsNullOrEmpty($value)) { Fail "$name - environment variable $var is not set" }
        $kind = 'text'
        $body = [Text.Encoding]::UTF8.GetBytes($value)
    } else {
        $path = Resolve-Path -LiteralPath $src -ErrorAction SilentlyContinue
        if (-not $path -or -not (Test-Path -LiteralPath $path.ProviderPath -PathType Leaf)) { Fail "$name - no file $src" }
        $kind = 'file'
        $body = [IO.File]::ReadAllBytes($path.ProviderPath)
    }
    try {
        $r = Invoke-RestMethod -Method Post -Uri "$CimonUrl/uploads?kind=$kind&label=$name" -Headers @{ Authorization = $auth } -ContentType 'application/octet-stream' -Body $body -UseBasicParsing
    } catch {
        $e = $_
        $msg = $e.ErrorDetails.Message
        try { $msg = ($msg | ConvertFrom-Json).message } catch {}
        if (-not $msg) { $msg = $e.Exception.Message }
        Fail "$name - the server refused: $msg"
    }
    $value = $null
    $fp = if ($r.sha256) { ', {0} bytes, sha256 {1}...' -f $r.bytes, $r.sha256.Substring(0, 12) } else { '' }
    [Console]::Error.WriteLine(('cimon upload: {0} ({1}{2}) - usable until {3}' -f $name, $kind, $fp, $r.expires_at))
    $handles[$name] = $r.handle
}
# pass this as `uploads` in the build/test/run call
$handles | ConvertTo-Json -Compress
