$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
try {
    Add-Type -AssemblyName System.Security
    $request = [Console]::In.ReadToEnd() | ConvertFrom-Json
    if ($request.id -cnotmatch '^[a-f0-9]{64}$') { throw 'invalid request' }
    $root = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'teams-cli\credentials'
    $path = Join-Path $root ($request.id + '.dpapi')
    $scope = [System.Security.Cryptography.DataProtectionScope]::CurrentUser
    $result = @{ status = 'ok'; secret = '' }
    switch ($request.action) {
        'get' {
            if (![IO.File]::Exists($path)) { $result.status = 'missing'; break }
            $encrypted = [IO.File]::ReadAllBytes($path)
            $plain = [System.Security.Cryptography.ProtectedData]::Unprotect($encrypted, $null, $scope)
            $result.secret = [Convert]::ToBase64String($plain)
        }
        'set' {
            $plain = [Convert]::FromBase64String($request.secret)
            $encrypted = [System.Security.Cryptography.ProtectedData]::Protect($plain, $null, $scope)
            [IO.Directory]::CreateDirectory($root) | Out-Null
            $temporary = Join-Path $root ([Guid]::NewGuid().ToString() + '.tmp')
            try {
                [IO.File]::WriteAllBytes($temporary, $encrypted)
                if ([IO.File]::Exists($path)) {
                    [IO.File]::Replace($temporary, $path, $null)
                } else {
                    [IO.File]::Move($temporary, $path)
                }
            } finally {
                if ([IO.File]::Exists($temporary)) { [IO.File]::Delete($temporary) }
            }
        }
        'delete' {
            if ([IO.File]::Exists($path)) { [IO.File]::Delete($path) }
            else { $result.status = 'missing' }
        }
        default { throw 'invalid action' }
    }
    [Console]::Out.Write(($result | ConvertTo-Json -Compress))
} catch {
    [Console]::Out.Write('{"status":"error"}')
    exit 1
}
