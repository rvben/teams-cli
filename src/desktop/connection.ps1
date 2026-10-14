# The request contains only a fixed action, port and restart boolean. This script is
# shared by native Windows and WSL so both use the same lock and recovery record.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$dir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'teams-cli'
$file = Join-Path $dir 'desktop-connection.json'
$key = 'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'
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
function Restore-Settings {
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
        if ($script:state.version -ne 1 -or $script:state.port -lt 1024 -or $null -eq $script:state.phase) { throw 'invalid_state' }
    }
    if ($request.action -eq 'status') { Result | ConvertTo-Json -Compress; return }
    if ($null -ne $script:state -and $script:state.port -ne $request.port) { throw 'different_port' }
    if ($null -ne $request.token -and $null -ne $script:state -and $request.token -cne $script:state.token) { throw 'busy' }
    switch ($request.action) {
        'connect' {
            if ($null -ne $script:state) { throw 'already_managed' }
            $previous = [Environment]::GetEnvironmentVariable($key, 'User')
            if ($previous -match '--remote-debugging-(port|address)(=|\s)') { throw 'existing_flags' }
            $installed = ($previous + ' --remote-debugging-port=' + $request.port + ' --remote-debugging-address=127.0.0.1').Trim()
            $script:state = [PSCustomObject]@{ version = 1; port = $request.port; previous = $previous; installed = $installed; settings_pending = $true; phase = 'starting'; token = $request.token; owners = @() }
            Save-State # Durable recovery record precedes every environment change.
            [Environment]::SetEnvironmentVariable($key, $installed, 'User')
            $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $installed
            foreach ($owner in (Processes)) { Stop-Process -Id $owner.id -ErrorAction Stop }
            Start-Sleep -Seconds 2
            Start-Process 'shell:AppsFolder\MSTeams_8wekyb3d8bbwe!MSTeams'
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
                if ($owned.Count -eq 0 -or $current.Count -ne $owned.Count) { throw 'session_changed' }
                foreach ($owner in $current) {
                    if (!($owned | Where-Object { $_.id -eq $owner.id -and $_.started -ceq $owner.started })) { throw 'session_changed' }
                }
                foreach ($owner in $current) { Stop-Process -Id $owner.id -ErrorAction Stop }
                Start-Sleep -Seconds 2
                # Set only this launch's environment; the previous user value is already restored.
                $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = $script:state.previous
                Start-Process 'shell:AppsFolder\MSTeams_8wekyb3d8bbwe!MSTeams'
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
    if ($code -notin @('busy','invalid_state','different_port','already_managed','existing_flags','missing_state','session_changed','settings_changed','not_managed','invalid_action')) { $code = 'operation_failed' }
    @{ error = $code } | ConvertTo-Json -Compress
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
