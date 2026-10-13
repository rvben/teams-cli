# Synthetic process/package metadata. No Teams process is inspected or launched.
$script:fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) 'TeamsFixture'
function Get-AppxPackage {
    param($Name)
    [pscustomobject]@{InstallLocation=$script:fixtureRoot}
}
function Get-NetTCPConnection {
    param($State, $ErrorAction)
    $address = if ($script:fixtureMode -eq 'unsafe') { '0.0.0.0' } else { '127.0.0.1' }
    [pscustomobject]@{LocalPort=$Port; LocalAddress=$address; OwningProcess=20}
}
function Get-CimInstance {
    param($ClassName, $Filter)
    if ($Filter -eq 'ProcessId=20') {
        [pscustomobject]@{Name='msedgewebview2.exe'; ParentProcessId=10; ExecutablePath='C:\Fixture\WebView.exe'}
    } else {
        $path = if ($script:fixtureMode -eq 'wrong_package') { Join-Path ([IO.Path]::GetTempPath()) 'Unrelated/ms-teams.exe' } else { Join-Path $script:fixtureRoot 'ms-teams.exe' }
        [pscustomobject]@{Name='ms-teams.exe'; ParentProcessId=0; ExecutablePath=$path}
    }
}
function Invoke-CimMethod {
    param($InputObject, $MethodName)
    $sid = if ($script:fixtureMode -eq 'wrong_user') { 'different-fixture-sid' } else { 'fixture-sid' }
    [pscustomobject]@{Sid=$sid}
}
