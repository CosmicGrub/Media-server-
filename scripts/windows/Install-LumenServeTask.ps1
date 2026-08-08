<#
.SYNOPSIS
    Installs `lumen serve` as a persistent Windows Scheduled Task.

.DESCRIPTION
    `lumen.exe serve <library>` is meant to run for as long as the machine is up, so a phone on the
    same LAN can pair with it at any time — but started from a console window, it dies the moment
    that window closes or the user signs out. This registers it as a Scheduled Task instead: it
    starts automatically at sign-in, restarts itself if it ever exits, and needs no console window
    left open.

    Uses only the built-in `ScheduledTasks` PowerShell module and calls nothing version-specific —
    this runs unchanged on Windows 10 and Windows 11, same as `lumen.exe` itself.

.PARAMETER LibraryPath
    Path to the media library `lumen serve` should serve. Required unless -Uninstall is given.

.PARAMETER LumenExe
    Path to lumen.exe. Defaults to a `lumen.exe` next to this script, which is where it ends up if
    you keep this script inside the release bundle `package-windows.sh` produces.

.PARAMETER Port
    TCP port to listen on. Matches `lumen serve`'s own default (7890) unless overridden.

.PARAMETER BindAddress
    Address to bind. Matches `lumen serve`'s own default (0.0.0.0 — every interface) unless
    overridden.

.PARAMETER TaskName
    Name for the Scheduled Task. Lets more than one library be served from the same machine on
    different ports, each under its own task name.

.PARAMETER Uninstall
    Remove a previously installed task instead of installing one. Only -TaskName is read in this
    mode.

.EXAMPLE
    .\Install-LumenServeTask.ps1 -LibraryPath "D:\Media"

    Serves D:\Media on the default port, starting now and every sign-in from now on.

.EXAMPLE
    .\Install-LumenServeTask.ps1 -LibraryPath "D:\Movies" -Port 7891 -TaskName LumenServeMovies

    A second, independent library served on a different port under its own task name.

.EXAMPLE
    .\Install-LumenServeTask.ps1 -Uninstall
    .\Install-LumenServeTask.ps1 -Uninstall -TaskName LumenServeMovies

    Stops and removes the task. The library on disk is untouched — this only removes the scheduled
    task, never any media.
#>

[CmdletBinding(DefaultParameterSetName = 'Install')]
param(
    [Parameter(ParameterSetName = 'Install', Mandatory = $true, Position = 0)]
    [string]$LibraryPath,

    [Parameter(ParameterSetName = 'Install')]
    [string]$LumenExe = (Join-Path $PSScriptRoot 'lumen.exe'),

    [Parameter(ParameterSetName = 'Install')]
    [ValidateRange(1, 65535)]
    [int]$Port = 7890,

    [Parameter(ParameterSetName = 'Install')]
    [string]$BindAddress = '0.0.0.0',

    [Parameter(ParameterSetName = 'Install')]
    [Parameter(ParameterSetName = 'Uninstall')]
    [string]$TaskName = 'LumenServe',

    [Parameter(ParameterSetName = 'Uninstall', Mandatory = $true)]
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'

if ($Uninstall) {
    $existingTask = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    if (-not $existingTask) {
        Write-Host "No scheduled task named '$TaskName' exists; nothing to remove."
        return
    }
    # Stop it first: Unregister-ScheduledTask removes the registration but does not itself kill a
    # currently running instance, which would otherwise keep the port bound with no task left to
    # show for it.
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
    Write-Host "Removed scheduled task '$TaskName'. lumen.exe and your media are untouched."
    return
}

if (-not (Get-Module -ListAvailable -Name ScheduledTasks)) {
    throw "The ScheduledTasks PowerShell module is not available on this machine."
}

$resolvedExe = Resolve-Path -Path $LumenExe -ErrorAction SilentlyContinue
if (-not $resolvedExe) {
    throw "lumen.exe not found at '$LumenExe'. Pass -LumenExe <path>, or run this script from " +
        "inside the folder lumen.exe was unzipped into."
}
$exePath = $resolvedExe.Path

$resolvedLibrary = Resolve-Path -Path $LibraryPath -ErrorAction SilentlyContinue
if (-not $resolvedLibrary) {
    throw "Library path '$LibraryPath' does not exist."
}
$libraryFull = $resolvedLibrary.Path

$workingDir = Split-Path -Parent $exePath
$argumentList = "serve `"$libraryFull`" --port $Port --bind $BindAddress"

Write-Host "Registering scheduled task '$TaskName':"
Write-Host "  exe:     $exePath"
Write-Host "  library: $libraryFull"
Write-Host "  port:    $Port"
Write-Host "  bind:    $BindAddress"

$action = New-ScheduledTaskAction -Execute $exePath -Argument $argumentList -WorkingDirectory $workingDir

# Starts at sign-in for whoever installs it. A machine-wide/boot-time trigger was deliberately not
# used: `lumen serve` launches mpv, which needs a desktop session to draw a window, so tying this to
# a user sign-in rather than the bare boot is what actually works rather than just what starts
# earliest.
$trigger = New-ScheduledTaskTrigger -AtLogOn

$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" `
    -LogonType Interactive -RunLevel Limited

$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries `
    -DontStopIfGoingOnBatteries `
    -StartWhenAvailable `
    -RestartCount 999 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit (New-TimeSpan -Seconds 0)   # 0 = no limit; `serve` is meant to run forever.

$existingTask = Get-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existingTask) {
    Write-Host "A task named '$TaskName' already exists; replacing it."
    Stop-ScheduledTask -TaskName $TaskName -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false
}

$description = "Runs 'lumen serve $libraryFull --port $Port', so a phone on this LAN can pair " + `
    "and control playback. Installed by Install-LumenServeTask.ps1."
Register-ScheduledTask -TaskName $TaskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Description $description | Out-Null

Write-Host "Starting it now..."
Start-ScheduledTask -TaskName $TaskName

Write-Host ""
Write-Host "Done. '$TaskName' starts automatically at sign-in and restarts itself if it exits."
Write-Host "Check it any time with:  Get-ScheduledTask -TaskName $TaskName | Get-ScheduledTaskInfo"
Write-Host "Remove it with:          .\Install-LumenServeTask.ps1 -Uninstall -TaskName $TaskName"
