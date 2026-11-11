# Run `omnimon --record` in the background whenever you log in to Windows,
# with no window. Output goes to %LOCALAPPDATA%\omnimon\recorder.log.
#
#   powershell -ExecutionPolicy Bypass -File scripts\recorder-windows.ps1           install and start
#   powershell -ExecutionPolicy Bypass -File scripts\recorder-windows.ps1 -Remove   stop and uninstall
#   ... recorder-windows.ps1 -Extra "--keep 30d"                                     with options
#
# Needs no admin rights: the task runs as you, at your logon.
param(
    [switch]$Remove,
    # The binary to run [default: omnimon on PATH, else the installer's location].
    [string]$Exe,
    # Extra arguments, e.g. "--keep 30d" or "--interval 2".
    [string]$Extra = ""
)
$ErrorActionPreference = "Stop"
$name = "omnimon recorder"

if ($Remove) {
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
    # Stopping the task ends conhost/cmd; end the recorder itself too.
    Get-CimInstance Win32_Process -Filter "Name = 'omnimon.exe'" |
        Where-Object { $_.CommandLine -match '--record' } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Confirm:$false }
    Write-Host "removed '$name'"
    return
}

if (-not $Exe) {
    $found = Get-Command omnimon -ErrorAction SilentlyContinue
    $Exe = if ($found) { $found.Source } else { "$env:LOCALAPPDATA\Programs\omnimon\omnimon.exe" }
}
if (-not (Test-Path $Exe)) { throw "omnimon not found at $Exe (pass -Exe PATH)" }
$Exe = (Resolve-Path $Exe).Path
$logDir = "$env:LOCALAPPDATA\omnimon"
New-Item -ItemType Directory -Force $logDir | Out-Null
$log = "$logDir\recorder.log"

# conhost --headless gives the console program a console without a window.
$action = New-ScheduledTaskAction -Execute "$env:WINDIR\System32\conhost.exe" `
    -Argument "--headless cmd.exe /c `"`"$Exe`" --record $Extra 2>>`"$log`"`""
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName $name -Description "omnimon --record (see omnimon --replay)" `
    -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName $name
Start-Sleep -Seconds 3
Write-Host "installed '$name': $Exe --record $Extra"
Get-Content $log -Tail 2 -ErrorAction SilentlyContinue
