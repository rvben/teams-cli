# Run on Windows: powershell.exe -NoProfile -File tests/wsl_credentials.test.ps1
$ErrorActionPreference = 'Stop'
$script = Get-Content (Join-Path $PSScriptRoot '../src/auth/wsl_credentials.ps1') -Raw
$encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
$sha = [Security.Cryptography.SHA256]::Create()
$id = ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes([Guid]::NewGuid().ToString())))).Replace('-', '').ToLowerInvariant()
$path = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) "teams-cli\credentials\$id.dpapi"
function Request($action, $secret = '') {
    $inputJson = @{ action = $action; id = $id; secret = $secret } | ConvertTo-Json -Compress
    $response = $inputJson | & powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand $encoded
    if ($LASTEXITCODE -ne 0) { throw "Credential request failed during $action" }
    return ($response | ConvertFrom-Json)
}
function Assert($condition, $message) {
    if (!$condition) { throw $message }
}
try {
    Assert ((Request 'get').status -eq 'missing') 'New entry should be missing'
    # Exceeds normal pipe and Credential Manager limits; includes non-ASCII bytes.
    $secret = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(('synthetic-token-' * 10000) + [char]0x20ac))
    Assert ((Request 'set' $secret).status -eq 'ok') 'Could not save credential'
    Assert ((Request 'get').secret -ceq $secret) 'Credential did not round-trip'
    $stored = [IO.File]::ReadAllBytes($path)
    Assert ([Text.Encoding]::UTF8.GetString($stored).IndexOf('synthetic-token-') -lt 0) 'Plaintext leaked to disk'
    $replacement = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes('replacement-token'))
    Assert ((Request 'set' $replacement).status -eq 'ok') 'Could not replace credential'
    Assert ((Request 'get').secret -ceq $replacement) 'Replacement did not round-trip'
    Assert ((Request 'delete').status -eq 'ok') 'Could not delete credential'
    Assert ((Request 'get').status -eq 'missing') 'Deleted credential still exists'
    Assert ((Request 'delete').status -eq 'missing') 'Missing delete should be distinguishable'
    Write-Output 'WSL credential DPAPI round-trip passed'
} finally {
    if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
    $sha.Dispose()
}
