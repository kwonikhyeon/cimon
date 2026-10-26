# cimon connect - sets up this PC to use the cimon build service from Claude Code.
# Served by the cimon server itself; the address below is that server's.
#   irm <server>/connect.ps1 | iex
# Asks for the invite code from `cimon invite`, stores the token DPAPI-encrypted for
# this Windows user, writes the header helper and registers cimon with Claude Code.
$ErrorActionPreference = 'Stop'
$CimonUrl = '{{URL}}'

function Fail([string]$m) { Write-Host "cimon connect: $m" -ForegroundColor Red; throw "cimon connect: $m" }

# claude.exe (native install) or npm's claude.cmd first: npm's claude.ps1 hands its
# arguments to node through Windows PowerShell, which breaks quotes inside them
$claude = Get-Command claude -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $claude) {
    $ps1 = Get-Command claude -CommandType ExternalScript -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($ps1) {
        $cmd = Join-Path (Split-Path $ps1.Source) 'claude.cmd'
        if (Test-Path $cmd) { $claude = Get-Command $cmd } else { Fail "only $($ps1.Source) was found - register by hand (docs/remote-use.md) or reinstall Claude Code" }
    }
}
if (-not $claude) { Fail 'Claude Code (claude) is not on PATH - install it first' }
$ver = ((& $claude.Source --version) | Out-String)
if ($ver -match '(\d+)\.(\d+)\.(\d+)') {
    if ([version]"$($matches[1]).$($matches[2]).$($matches[3])" -lt [version]'2.1.304') { Fail "Claude Code $($matches[0]) is too old - run: claude update (MCP tasks need 2.1.304 or newer)" }
}

# Windows PowerShell 5.1 does not quote arguments for native programs: quote them
# here (MSVCRT rules), and go through cmd for an npm .cmd shim.
function Quote-Arg([string]$a) {
    if ($a -ne '' -and $a -notmatch '[\s"]') { return $a }
    return '"' + (($a -replace '(\\*)"', '$1$1\"') -replace '(\\+)$', '$1$1') + '"'
}
# Returns the exit code; with -Capture, the output instead (stdout and stderr).
function Invoke-Claude([string[]]$ArgList, [switch]$Quiet, [switch]$Capture) {
    $line = ($ArgList | ForEach-Object { Quote-Arg $_ }) -join ' '
    $file = $claude.Source
    if ($claude.Source -match '\.(cmd|bat)$') { $file = $env:ComSpec; $line = '/d /s /c ""' + $claude.Source + '" ' + $line + '"' }
    $o = @{ FilePath = $file; ArgumentList = $line; NoNewWindow = $true; Wait = $true; PassThru = $true }
    if ($Quiet -or $Capture) { $o.RedirectStandardOutput = [IO.Path]::GetTempFileName(); $o.RedirectStandardError = [IO.Path]::GetTempFileName() }
    $p = Start-Process @o
    $text = ''
    if ($Capture) { $text = (Get-Content $o.RedirectStandardOutput -Raw) + (Get-Content $o.RedirectStandardError -Raw) }
    if ($Quiet -or $Capture) { Remove-Item $o.RedirectStandardOutput, $o.RedirectStandardError -ErrorAction SilentlyContinue }
    if ($Capture) { return [string]$text }
    return $p.ExitCode
}

# CIMON_INVITE_CODE lets a provisioning script pass the code without a prompt
$code = if ($env:CIMON_INVITE_CODE) { $env:CIMON_INVITE_CODE.Trim() } else { (Read-Host 'cimon invite code').Trim() }
$body = @{ code = $code; device = $env:COMPUTERNAME } | ConvertTo-Json -Compress
try { $r = Invoke-RestMethod -Method Post -Uri "$CimonUrl/connect/redeem" -ContentType 'application/json' -Body $body -UseBasicParsing }
catch {
    # the server says why: a wrong or used code, or too many attempts
    $e = $_
    $msg = $e.ErrorDetails.Message
    try { $msg = ($msg | ConvertFrom-Json).message } catch {}
    if (-not $msg) { $msg = $e.Exception.Message }
    Fail "the server refused: $msg"
}

$dir = Join-Path $env:USERPROFILE '.cimon'
New-Item -ItemType Directory -Force $dir | Out-Null
ConvertTo-SecureString $r.token -AsPlainText -Force | ConvertFrom-SecureString | Set-Content (Join-Path $dir 'token.dpapi')
$helper = Join-Path $dir 'header.ps1'
Set-Content -Path $helper -Encoding ASCII -Value @'
$secure = Get-Content (Join-Path $env:USERPROFILE '.cimon\token.dpapi') | ConvertTo-SecureString
$token = [System.Net.NetworkCredential]::new('', $secure).Password
@{ Authorization = "Bearer $token" } | ConvertTo-Json -Compress
'@
# for steps that need a file from this PC (a signing key): upload.ps1 next to the helper
try { Invoke-WebRequest -Uri "$CimonUrl/upload.ps1" -OutFile (Join-Path $dir 'upload.ps1') -UseBasicParsing }
catch { Write-Host "cimon connect: could not fetch upload.ps1 - get it later from $CimonUrl/upload.ps1" -ForegroundColor Yellow }

$helperCmd = 'powershell -NoProfile -ExecutionPolicy Bypass -File "' + ($helper -replace '\\', '/') + '"'
$json = @{ type = 'http'; url = "$CimonUrl/mcp"; headersHelper = $helperCmd } | ConvertTo-Json -Compress
Invoke-Claude @('mcp', 'remove', '--scope', 'user', 'cimon') -Quiet | Out-Null   # replace an earlier registration
$rc = Invoke-Claude @('mcp', 'add-json', '--scope', 'user', 'cimon', $json)
if ($rc -ne 0) { Fail "claude mcp add-json exited $rc" }
# what Claude Code stored must be what we meant (an argument mangled on the way would
# register something else and still exit 0), and its own connection check — through the
# header helper, with the token — must pass
$got = Invoke-Claude @('mcp', 'get', 'cimon') -Capture
if ($got -notmatch [regex]::Escape("$CimonUrl/mcp") -or $got -cnotmatch '\bConnected\b') { Fail "claude mcp get cimon does not show a connected server at $CimonUrl/mcp:`n$got" }

try { $h = Invoke-RestMethod -Uri "$CimonUrl/healthz" -Headers @{ Authorization = "Bearer $($r.token)" } -UseBasicParsing }
catch { Fail "registered, but the server did not accept the token: $($_.Exception.Message)" }
if (-not $h.PSObject.Properties['queued']) { Fail 'registered, but /healthz did not answer as a signed-in user' }
$user = $r.user
Remove-Variable r
Write-Host ''
Write-Host "cimon connect: done - signed in as $user." -ForegroundColor Green
Write-Host 'Start Claude Code and ask, for example: cimon project list'
