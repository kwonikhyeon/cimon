# cimon header helper - Claude Code runs this for every connection to cimon
# (headersHelper) and sends the header it prints. Served by the cimon server itself
# ({{URL}}/header.ps1); the connect script keeps it in %USERPROFILE%\.cimon\header.ps1.
#
# .NET calls only, no cmdlets: Windows PowerShell 5.1 started from PowerShell 7 inherits
# 7's module path and cannot load its own modules there (ConvertTo-SecureString "could
# not be loaded"). And when it cannot read the token it fails, with the reason on
# stderr - an empty token would only come back as the server's 401.
$ErrorActionPreference = 'Stop'
try {
    $path = [IO.Path]::Combine($env:USERPROFILE, '.cimon', 'token.dpapi')
    if (-not [IO.File]::Exists($path)) { throw "no $path" }
    # what ConvertFrom-SecureString writes: hex of a DPAPI blob (this Windows user) of
    # the UTF-16 token
    $hex = [IO.File]::ReadAllText($path).Trim()
    if ($hex -notmatch '^(?:[0-9a-fA-F]{2})+$') { throw "$path is not a DPAPI blob in hex" }
    $blob = [byte[]]::new($hex.Length / 2)
    for ($i = 0; $i -lt $blob.Length; $i++) { $blob[$i] = [Convert]::ToByte($hex.Substring(2 * $i, 2), 16) }
    [void][Reflection.Assembly]::Load('System.Security, Version=4.0.0.0, Culture=neutral, PublicKeyToken=b03f5f7f11d50a3a')
    try { $plain = [Security.Cryptography.ProtectedData]::Unprotect($blob, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser) }
    catch { throw "cannot decrypt $path as this Windows user ($($_.Exception.InnerException.Message.Trim()))" }
    $token = [Text.Encoding]::Unicode.GetString($plain).Trim()
    [Array]::Clear($plain, 0, $plain.Length)
    # an RFC 6750 token: nothing in it needs escaping in JSON
    if ($token -notmatch '^[A-Za-z0-9._~+/-]+=*$') { throw "the token in $path is empty or malformed" }
    [Console]::Out.Write('{"Authorization":"Bearer ' + $token + '"}')
} catch {
    [Console]::Error.WriteLine("cimon header helper: $($_.Exception.Message) - if the token is gone, ask the cimon admin for an invite and connect again: irm {{URL}}/connect.ps1 | iex")
    exit 1
}
