# Exercise the production state machine with isolated files and fake platform effects.
# No user environment value or Teams process may be touched by these fixtures.
$ErrorActionPreference = 'Stop'
$originalProcessFlags = [Environment]::GetEnvironmentVariable('WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS', 'Process')
$sourcePath = Join-Path $PSScriptRoot '..\src\desktop\connection.ps1'
$source = [IO.File]::ReadAllText($sourcePath)
$testDir = Join-Path ([IO.Path]::GetTempPath()) ('teams-cli-fixture-' + [Guid]::NewGuid().ToString('N'))
$source = $source.Replace("'Local\teams-cli-desktop-'", "'Local\teams-cli-fixture-$([Guid]::NewGuid().ToString('N'))-'")
$replacements = @(
    @('$dir = Join-Path ([Environment]::GetFolderPath(''LocalApplicationData'')) ''teams-cli''', '$dir = $script:testDir'),
    @('[Environment]::GetEnvironmentVariable($key, ''User'')', '(Test-GetFlags)'),
    @('[Environment]::SetEnvironmentVariable($key, $script:state.previous, ''User'')', 'Test-SetFlags $script:state.previous'),
    @('[Environment]::SetEnvironmentVariable($key, $installed, ''User'')', 'Test-SetFlags $installed'),
    @('Stop-Process -Id $owner.id -ErrorAction Stop', 'Test-StopTeams $owner.id'),
    @('Start-Process ''shell:AppsFolder\MSTeams_8wekyb3d8bbwe!MSTeams''', 'Test-StartTeams')
)
foreach ($pair in $replacements) {
    if (!$source.Contains($pair[0])) { throw 'fixture_adapter_no_longer_matches_production' }
    $source = $source.Replace($pair[0], $pair[1])
}
# Guard against accidentally leaving any dangerous platform operation active.
if ($source.Contains('[Environment]::SetEnvironmentVariable') -or $source.Contains('Stop-Process -Id') -or $source.Contains("Start-Process 'shell:")) { throw 'unsafe_fixture_adapter' }
$marker = "try {`n    try { `$locked"
if (!$source.Contains($marker)) { throw 'fixture_override_point_changed' }
$source = $source.Replace($marker, "function Processes { `$script:processes }`nfunction Listener-Owners { if (`$script:listenerRoot) { `$script:listenerRoot } }`nfunction Start-Sleep { }`n" + $marker)
$script:flags = $null
$script:processes = @([PSCustomObject]@{id=1;started='original'})
$script:starts = 0
$script:stops = 0
$script:failStart = $false
function Test-GetFlags { $script:flags }
function Test-SetFlags($value) { $script:flags = $value }
function Test-StopTeams($processId) {
    $script:stops++
    $script:processes = @($script:processes | Where-Object { $_.id -ne $processId })
    if ($script:listenerRoot -eq $processId) { $script:listenerRoot = $script:replacementListener }
}
function Test-StartTeams {
    if ($script:failStart) { throw 'private_platform_error_must_not_escape' }
    $script:starts++
    $script:processes += @([PSCustomObject]@{id=100+$script:starts;started=('generation'+$script:starts)})
    $script:listenerRoot = $(if ($env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS -match '--remote-debugging-port=') { 100+$script:starts } else { 0 })
}
function Invoke-Operation($action, $port=19222, $token=$null, $restart=$false) {
    $request = [PSCustomObject]@{action=$action;port=$port;token=$token;restart=$restart}
    $text = & ([scriptblock]::Create($source))
    $value = ($text -join "`n") | ConvertFrom-Json
    if ($null -eq $value) { throw 'missing_fixture_result' }
    $value
}
function Check($condition, $name) { if (!$condition) { throw ('fixture_failed_'+$name) }; $script:checks++ }
function Reset-Fixture($prior=$null) {
    if (Test-Path -LiteralPath $testDir) { Remove-Item -Recurse -LiteralPath $testDir }
    $script:flags=$prior; $script:processes=@([PSCustomObject]@{id=1;started='original'}); $script:starts=0; $script:stops=0; $script:failStart=$false; $script:listenerRoot=0; $script:replacementListener=0
}
$script:checks = 0
try {
    foreach ($prior in @($null,'--disable-features=FixturePrivateValue')) {
        Reset-Fixture $prior
        $r=Invoke-Operation 'connect' 19222 'owner-a'
        Check ($r.settings_pending -and $r.phase -eq 'starting') 'connect_saved_before_launch'
        Check ($script:flags.Contains('--remote-debugging-port=19222')) 'debug_flags_installed'
        Check ((Invoke-Operation 'recover' 19222 'owner-b').error -eq 'busy') 'competing_owner_refused'
        Check ((Invoke-Operation 'disconnect').error -eq 'busy') 'in_progress_disconnect_refused'
        Check ((Invoke-Operation 'finish' 19223 'owner-a').error -eq 'different_port') 'wrong_port_refused'
        $r=Invoke-Operation 'finish' 19222 'owner-a'
        Check ($r.phase -eq 'active' -and !$r.settings_pending) 'finished_ready'
        Check ($script:flags -ceq $prior) 'previous_flags_restored'
        $json=$r|ConvertTo-Json -Compress
        Check (!$json.Contains('FixturePrivateValue') -and !$json.Contains('owner-a')) 'public_result_redacted'
        $r=Invoke-Operation 'disconnect' 19222 $null $true
        Check (!$r.managed -and $script:starts -eq 2) 'disconnect_owned_process'
        Check ($script:flags -ceq $prior) 'disconnect_preserves_prior_flags'
    }
    Reset-Fixture 'prior'
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $installed=$script:flags
    $script:flags='external'
    Check ((Invoke-Operation 'finish' 19222 'owner-a').error -eq 'settings_changed') 'external_change_refused'
    Check ((Invoke-Operation 'recover').error -eq 'settings_changed') 'external_change_preserved_by_recovery'
    Check ($script:flags -ceq 'external') 'external_not_overwritten'
    $script:flags=$installed
    $r=Invoke-Operation 'recover'
    Check ($script:flags -ceq 'prior' -and !$r.settings_pending) 'interrupted_settings_restored'
    Check ((Invoke-Operation 'disconnect' 19222 $null $true).error -eq 'session_changed') 'unrecorded_process_not_adopted'
    $null=Invoke-Operation 'disconnect' 19222 $null $false
    Reset-Fixture 'prior'
    $script:failStart=$true
    Check ((Invoke-Operation 'connect' 19222 'owner-a').error -eq 'operation_failed') 'platform_exception_redacted'
    $r=Invoke-Operation 'recover'
    Check ($script:flags -ceq 'prior' -and !$r.settings_pending) 'failed_launch_recoverable'
    Reset-Fixture
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $null=Invoke-Operation 'finish' 19222 'owner-a'
    $script:processes=@([PSCustomObject]@{id=999;started='independent-restart'})
    $script:listenerRoot=999
    Check ((Invoke-Operation 'disconnect' 19222 $null $true).error -eq 'session_changed') 'changed_process_preserved'
    Check ($script:starts -eq 1) 'changed_process_not_restarted'
    Reset-Fixture
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $null=Invoke-Operation 'finish' 19222 'owner-a'
    $script:processes+=@([PSCustomObject]@{id=999;started='auxiliary'})
    $r=Invoke-Operation 'disconnect' 19222 $null $true
    Check (!$r.managed -and @($script:processes|Where-Object id -eq 999).Count -eq 1) 'additional_process_preserved'
    Reset-Fixture
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $script:processes+=@([PSCustomObject]@{id=999;started='temporary'})
    $null=Invoke-Operation 'finish' 19222 'owner-a'
    $script:processes=@($script:processes|Where-Object id -ne 999)
    Check (!(Invoke-Operation 'disconnect' 19222 $null $true).managed) 'exited_auxiliary_does_not_block_listener_owner'
    Reset-Fixture
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $null=Invoke-Operation 'finish' 19222 'owner-a'
    $script:processes+=@([PSCustomObject]@{id=999;started='independent'})
    $script:listenerRoot=999
    Check ((Invoke-Operation 'disconnect' 19222 $null $true).error -eq 'session_changed' -and $script:stops -eq 1) 'unowned_listener_not_adopted'
    Reset-Fixture
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $null=Invoke-Operation 'finish' 19222 'owner-a'
    $script:processes=@([PSCustomObject]@{id=101;started='reused-pid'})
    Check ((Invoke-Operation 'disconnect' 19222 $null $true).error -eq 'session_changed' -and $script:stops -eq 1) 'reused_pid_not_stopped'
    Reset-Fixture
    $null=Invoke-Operation 'connect' 19222 'owner-a'
    $null=Invoke-Operation 'finish' 19222 'owner-a'
    $script:replacementListener=999
    Check ((Invoke-Operation 'disconnect' 19222 $null $true).error -eq 'session_changed' -and (Invoke-Operation 'status').managed) 'port_rebound_keeps_recovery_record'
    Reset-Fixture '--remote-debugging-port=9333'
    Check ((Invoke-Operation 'connect' 19222 'owner-a').error -eq 'existing_flags') 'existing_debug_session_preserved'
    Check ($script:flags -ceq '--remote-debugging-port=9333' -and $script:starts -eq 0) 'existing_flags_untouched'
    Reset-Fixture 'independent-value'
    Check (!(Invoke-Operation 'recover').managed -and $script:flags -ceq 'independent-value') 'idle_recovery_has_no_effect'
    Check (!(Invoke-Operation 'disconnect').managed -and $script:starts -eq 0 -and $script:flags -ceq 'independent-value') 'idle_disconnect_has_no_effect'
    Check ((Invoke-Operation 'disconnect' 19222 $null $true).error -eq 'not_managed' -and $script:starts -eq 0) 'unmanaged_process_not_restarted'
    [PSCustomObject]@{status='passed';checks=$script:checks;real_user_environment_changes=0;real_teams_restarts=0}|ConvertTo-Json -Compress
} finally {
    [Environment]::SetEnvironmentVariable('WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS', $originalProcessFlags, 'Process')
    if (Test-Path -LiteralPath $testDir) { Remove-Item -Recurse -LiteralPath $testDir }
}
