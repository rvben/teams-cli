# Fixed read-only helper. Input is JSON data; no user text is evaluated as code.
[Console]::InputEncoding = New-Object Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
try {
    $requestData = [Console]::In.ReadToEnd()
    if ($requestData.Length -gt 4096) { throw 'Invalid request' }
    $inputData = $requestData | ConvertFrom-Json
    if ($inputData.operation -ne 'probe' -or ($inputData.port -isnot [int] -and $inputData.port -isnot [long]) -or
        $inputData.port -lt 1024 -or $inputData.port -gt 65535) { throw 'Invalid request' }
    $Port = [int]$inputData.port
} catch {
    [Console]::Out.WriteLine('{"error":{"kind":"invalid_input","message":"Expected a probe request with a port between 1024 and 65535"}}')
    exit 1
}

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

function Read-LocalJson([string]$Route) {
    $request = [Net.HttpWebRequest]::Create("http://127.0.0.1:$Port$Route")
    $request.Proxy = $null
    $request.AllowAutoRedirect = $false
    $request.Timeout = 5000
    $request.ReadWriteTimeout = 5000
    $request.UserAgent = 'TeamsCLI-DesktopProbe/0.1'
    $response = $request.GetResponse()
    try {
        if ([int]$response.StatusCode -ne 200) { throw 'Unexpected HTTP status' }
        $reader = New-Object IO.StreamReader($response.GetResponseStream())
        try {
            $buffer = New-Object char[] 1048577
            $length = $reader.ReadBlock($buffer, 0, $buffer.Length)
            if ($length -eq 0 -or $length -gt 1048576) { throw 'Invalid metadata size' }
            $document = -join $buffer[0..($length-1)]
            $start = $document.TrimStart()
            if (($Route -eq '/json/version' -and -not $start.StartsWith('{')) -or
                ($Route -eq '/json/list' -and -not $start.StartsWith('['))) { throw 'Invalid metadata shape' }
            $parsed = $document | ConvertFrom-Json
            return $parsed
        } finally { $reader.Dispose() }
    } finally { $response.Dispose() }
}

function Test-TeamsOwner([int]$OwnerId) {
    # Process ancestry only; never read command lines, titles, caches or credentials.
    $seen = @{}
    for ($i = 0; $i -lt 32 -and $OwnerId -gt 0; $i++) {
        if ($seen.ContainsKey($OwnerId)) { break }
        $seen[$OwnerId] = $true
        $process = Get-CimInstance Win32_Process -Filter "ProcessId=$OwnerId"
        if (-not $process) { break }
        if ($process.Name -ieq 'ms-teams.exe') {
            if ($script:teamsExecutables -inotcontains $process.ExecutablePath) { return $false }
            $owner = Invoke-CimMethod -InputObject $process -MethodName GetOwnerSid
            return $owner.Sid -eq $script:currentSid
        }
        $OwnerId = [int]$process.ParentProcessId
    }
    return $false
}

$result = [ordered]@{
    schema = 'teams-desktop-metadata-v1'
    checked_utc = [DateTime]::UtcNow.ToString('o')
    port = $Port
    status = 'no_listener'
    loopback_only = $null
    teams_owned = $null
    version = $null
    browser_websocket_advertised = $false
    targets = @()
}
try {
    $script:currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $script:teamsExecutables = @(Get-AppxPackage -Name MSTeams | ForEach-Object {
        Join-Path $_.InstallLocation 'ms-teams.exe'
    })
    $listeners = @(Get-NetTCPConnection -State Listen -ErrorAction Stop |
        Where-Object LocalPort -eq $Port)
    if ($listeners.Count) {
        $result.loopback_only = @($listeners | Where-Object {
            $_.LocalAddress -notin @('127.0.0.1','::1')
        }).Count -eq 0
        $owned = @($listeners | ForEach-Object { Test-TeamsOwner $_.OwningProcess })
        $result.teams_owned = ($owned -notcontains $false)
        if (-not $result.loopback_only) { $result.status = 'unsafe_listener_refused' }
        elseif (-not $result.teams_owned) { $result.status = 'non_teams_listener_refused' }
        else {
            $version = Read-LocalJson '/json/version'
            $targets = @(Read-LocalJson '/json/list')
            $result.version = [ordered]@{
                browser = [string]$version.Browser
                protocol = [string]$version.'Protocol-Version'
            }
            $result.browser_websocket_advertised = [bool]$version.webSocketDebuggerUrl
            $result.targets = @($targets | ForEach-Object {
                $isTeams = $false
                $origin = '[other origin omitted]'
                $scheme = ''
                try {
                    $uri = [Uri]$_.url
                    $scheme = $uri.Scheme
                    $isTeams = ($uri.Scheme -eq 'https' -and $uri.DnsSafeHost -in @('teams.microsoft.com','teams.cloud.microsoft','teams.live.com') -and $uri.Port -eq 443)
                    if ($isTeams -or ($uri.Scheme -eq 'https' -and $uri.DnsSafeHost -in @('login.microsoftonline.com','login.live.com') -and $uri.Port -eq 443)) {
                        $origin = 'https://' + $uri.DnsSafeHost
                    } elseif ($uri.Scheme -in @('file','blob','about')) { $origin = $uri.Scheme + ':' }
                } catch { }
                # No target titles, URLs, IDs or WebSocket URLs leave this function.
                [ordered]@{
                    type = [string]$_.type
                    url_scheme = $scheme
                    url_origin = $origin
                    teams_origin = $isTeams
                    websocket_advertised = [bool]$_.webSocketDebuggerUrl
                }
            })
            $result.status = 'metadata_ok'
        }
    }
} catch { $result.status = 'metadata_probe_failed' }
$result | ConvertTo-Json -Depth 8 -Compress
