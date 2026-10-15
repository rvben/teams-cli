# The request contains only a fixed action, port and restart boolean. This script is
# shared by native Windows and WSL so both use the same lock and recovery record.
# Debugging flags are given to the Teams launch only. A user-wide value would reach
# every WebView2 application (Outlook, Widgets, ...) started while it is set.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$dir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'teams-cli'
$file = Join-Path $dir 'desktop-connection.json'
$key = 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'
# The app execution alias passes this process's environment on to Teams;
# shell:AppsFolder activation would not.
$launcher = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Microsoft\WindowsApps\ms-teams.exe'
$mutex = [Threading.Mutex]::new($false, ('Local\teams-cli-desktop-' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value))
$locked = $false
function Save-State {
    [IO.Directory]::CreateDirectory($dir) | Out-Null
    $tmp = $file + '.' + [Guid]::NewGuid().ToString('N')
    try {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($script:state | ConvertTo-Json -Depth 5))
        $stream = [IO.File]::Open($tmp, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $stream.Write($bytes, 0, $bytes.Length); $stream.Flush($true) } finally { $stream.Dispose() }
        # PowerShell 5.1 converts $null to an empty string for String arguments.
        if (Test-Path -LiteralPath $file) { [IO.File]::Replace($tmp, $file, [System.Management.Automation.Language.NullString]::Value) }
        else { [IO.File]::Move($tmp, $file) }
    } finally { if (Test-Path -LiteralPath $tmp) { Remove-Item -LiteralPath $tmp } }
}
function Processes {
    @(Get-Process ms-teams -ErrorAction SilentlyContinue | ForEach-Object {
        # Only processes in this desktop session can be managed by this command.
        if ($_.SessionId -eq [Diagnostics.Process]::GetCurrentProcess().SessionId) {
            [PSCustomObject]@{ id = $_.Id; started = $_.StartTime.ToUniversalTime().Ticks.ToString() }
        }
    })
}
function Listener-Owners([int]$port) {
    $listeners = @(Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue)
    if ($listeners.Count -eq 0) { return }
    $processes = @{}
    foreach ($process in (Get-CimInstance Win32_Process)) { $processes[[int]$process.ProcessId] = $process }
    foreach ($listener in $listeners) {
        $processId = [int]$listener.OwningProcess
        $seen = @{}
        $found = $false
        for ($depth = 0; $depth -lt 32; $depth++) {
            if ($processId -eq 0 -or $seen.ContainsKey($processId) -or !$processes.ContainsKey($processId)) { break }
            $seen[$processId] = $true
            $process = $processes[$processId]
            if ($process.Name -ieq 'ms-teams.exe') { $processId; $found = $true; break }
            $processId = [int]$process.ParentProcessId
        }
        if (!$found) { throw 'session_changed' }
    }
}
function Start-Teams([string]$flags) {
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $flags
    Start-Process $launcher
}
function Without-Debugging([string]$value) {
    ($value -replace '\s*(?<!\S)--remote-debugging-(port|address)(=\S*|\s+(?!-)\S+)?(?!\S)', '').Trim()
}
# Only records written by older versions changed the user-wide value.
function Restore-Settings {
    if (!$script:state.settings_pending) { return }
    $current = [Environment]::GetEnvironmentVariable($key, 'User')
    if ($current -ceq $script:state.installed) {
        [Environment]::SetEnvironmentVariable($key, $script:state.previous, 'User')
    } elseif ($current -cne $script:state.previous) { throw 'settings_changed' }
    $script:state.settings_pending = $false
    Save-State
}
function Result {
    [PSCustomObject]@{
        managed = ($null -ne $script:state)
        port = $(if ($null -ne $script:state) { $script:state.port } else { $null })
        phase = $(if ($null -ne $script:state) { $script:state.phase } else { 'idle' })
        settings_pending = ($null -ne $script:state -and $script:state.settings_pending)
        needs_recovery = ($null -ne $script:state -and ($script:state.settings_pending -or $script:state.phase -ne 'active'))
    }
}
try {
    try { $locked = $mutex.WaitOne(1000) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) { throw 'busy' }
    $script:state = $null
    if (Test-Path -LiteralPath $file) {
        $script:state = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json
        if ($script:state.version -notin 1,2 -or $script:state.port -lt 1024 -or $null -eq $script:state.phase) { throw 'invalid_state' }
    }
    if ($request.action -eq 'status') { Result | ConvertTo-Json -Compress; return }
    if ($request.action -eq 'clear_flags') {
        # A legacy record still owns the user-wide value until it is recovered.
        if ($null -ne $script:state -and $script:state.settings_pending) { throw 'already_managed' }
        $current = [Environment]::GetEnvironmentVariable($key, 'User')
        $cleared = Without-Debugging $current
        $changed = $cleared -cne [string]$current
        if ($changed) { [Environment]::SetEnvironmentVariable($key, $cleared, 'User') } # Empty removes it.
        Result | Add-Member -NotePropertyName cleared -NotePropertyValue $changed -PassThru | ConvertTo-Json -Compress
        return
    }
    if ($null -ne $script:state -and $script:state.port -ne $request.port) { throw 'different_port' }
    if ($null -ne $request.token -and $null -ne $script:state -and $request.token -cne $script:state.token) { throw 'busy' }
    switch ($request.action) {
        'connect' {
            if ($null -ne $script:state) { throw 'already_managed' }
            $own = [Environment]::GetEnvironmentVariable($key, 'User')
            if ($own -match '(?<!\S)--remote-debugging-(port|address)(=|\s|$)') { throw 'existing_flags' }
            if (!(Test-Path -LiteralPath $launcher)) { throw 'launcher_missing' }
            $script:state = [PSCustomObject]@{ version = 2; port = $request.port; phase = 'starting'; token = $request.token; owners = @() }
            Save-State # Durable recovery record precedes stopping Teams.
            foreach ($owner in (Processes)) { Stop-Process -Id $owner.id -ErrorAction Stop }
            Start-Sleep -Seconds 2
            Start-Teams (($own + ' --remote-debugging-port=' + $request.port + ' --remote-debugging-address=127.0.0.1').Trim())
        }
        'finish' {
            if ($null -eq $script:state) { throw 'missing_state' }
            if ($script:state.phase -ne 'starting') { throw 'busy' }
            $script:state.owners = @(Processes)
            if ($script:state.owners.Count -eq 0) { throw 'session_changed' }
            Restore-Settings
            $script:state.phase = 'active'
            Save-State
        }
        'recover' {
            if ($null -ne $script:state) { Restore-Settings; if (@($script:state.owners).Count -gt 0) { $script:state.phase = 'active' } else { $script:state.phase = 'interrupted' }; Save-State }
        }
        'disconnect' {
            if ($null -eq $script:state) {
                if ($request.restart) { throw 'not_managed' }
                Result | ConvertTo-Json -Compress
                return
            }
            if ($script:state.phase -eq 'starting') { throw 'busy' }
            Restore-Settings
            if ($request.restart) {
                $current = @(Processes)
                $owned = @($script:state.owners)
                $matching = @($current | Where-Object {
                    $candidate = $_
                    @($owned | Where-Object { $_.id -eq $candidate.id -and $_.started -ceq $candidate.started }).Count -gt 0
                })
                $listeners = @(Listener-Owners $request.port)
                if ($matching.Count -eq 0 -or $listeners.Count -eq 0) { throw 'session_changed' }
                foreach ($processId in $listeners) {
                    if (!($matching | Where-Object { $_.id -eq $processId })) { throw 'session_changed' }
                }
                # Teams may add auxiliary processes after startup. Only recorded
                # identities are ours; the verified listener must still belong to one.
                foreach ($owner in $matching) { Stop-Process -Id $owner.id -ErrorAction Stop }
                Start-Sleep -Seconds 2
                if (@(Listener-Owners $request.port).Count -gt 0) { throw 'session_changed' }
                # Any legacy user-wide change is already restored above.
                Start-Teams ([Environment]::GetEnvironmentVariable($key, 'User'))
            }
            Remove-Item -LiteralPath $file
            $script:state = $null
        }
        default { throw 'invalid_action' }
    }
    Result | ConvertTo-Json -Compress
} catch {
    # Never return arbitrary PowerShell exceptions: they can contain private settings.
    $code = $_.Exception.Message
    if ($code -notin @('busy','invalid_state','different_port','already_managed','existing_flags','launcher_missing','missing_state','session_changed','settings_changed','not_managed','invalid_action')) { $code = 'operation_failed' }
    @{ error = $code } | ConvertTo-Json -Compress
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
