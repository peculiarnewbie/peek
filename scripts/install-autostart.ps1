<#
Sets peek up as a background service for this user. It:

  - allows peek.exe through Windows Firewall, from private and Tailscale
    addresses only;
  - registers a logon task that starts peek hidden and as administrator.

Running as administrator is what lets it control windows that are themselves
elevated (Task Manager, installers, admin terminals). The scheduled task is
allowed to start elevated without a UAC prompt each time.

`peek install` runs this script for you (with -Config). By hand, from an
elevated PowerShell in a source checkout:
    .\scripts\install-autostart.ps1              # install and start now
    .\scripts\install-autostart.ps1 -Remove      # uninstall
    .\scripts\install-autostart.ps1 -PeekArgs '--port 9000 --no-clipboard'
    .\scripts\install-autostart.ps1 -PeekArgs '--no-token'   # Tailscale only, no token
#>
#Requires -RunAsAdministrator
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\peek.exe'),
    [string]$PeekArgs = '',
    [switch]$Remove,
    # A JSON file from `peek install` / `peek uninstall`. It names the user
    # and paths explicitly: the elevated script may run as a different account
    # (a standard user types an administrator's password at the UAC prompt).
    [string]$Config
)
$ErrorActionPreference = 'Stop'
$name = 'peek'
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$log = Join-Path $env:APPDATA 'peek\peek.log'
$source = $null
$callerPid = 0
$output = $null

if ($Config) {
    $c = Get-Content -Raw -LiteralPath $Config | ConvertFrom-Json
    $Exe = $c.exe
    $Remove = [bool]$c.remove
    $callerPid = [int]$c.callerPid
    $output = $c.output
    if ($c.source) { $source = $c.source }
    if ($c.user) { $user = $c.user }
    if ($c.log) { $log = $c.log }
    # Quote each flag that needs it, for the task's command line.
    $PeekArgs = (@($c.peekArgs) | Where-Object { $_ -ne $null } | ForEach-Object {
        if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ }
    }) -join ' '
}
$exePath = [System.IO.Path]::GetFullPath($Exe)

# Messages go to the console, and to a file when `peek install` asked for one
# (an elevated window cannot write to the console that started it).
function Say([string]$line) {
    $line
    if ($output) { Add-Content -LiteralPath $output -Value $line -Encoding UTF8 }
}

# Windows adds "block" rules of its own the first time a program listens and
# nobody answers the prompt; those would override an allow rule, so clear
# everything that refers to this executable first, and earlier peek rules
# for another location.
function Remove-PeekFirewallRules {
    Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue |
        Where-Object { $_.Program -eq $exePath } |
        Get-NetFirewallRule | Remove-NetFirewallRule
    Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}

# Every running peek except the `peek install` that started this script.
function Stop-Peek {
    $running = @(Get-Process -Name peek -ErrorAction SilentlyContinue | Where-Object { $_.Id -ne $callerPid })
    $running | Stop-Process -Force
    $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

try {
    if ($Remove) {
        Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
        Stop-Peek
        Remove-PeekFirewallRules
        if ($Config -and (Test-Path -LiteralPath $exePath)) {
            Remove-Item -LiteralPath $exePath -Force
            Remove-Item -LiteralPath (Split-Path $exePath) -ErrorAction SilentlyContinue
        }
        Say "Removed the '$name' logon task and firewall rule, and stopped peek."
        return
    }

    # Replace the installed copy: stop whatever runs it first, since Windows
    # will not overwrite a running executable.
    Stop-Peek
    if ($source -and ([System.IO.Path]::GetFullPath($source) -ne $exePath)) {
        New-Item -ItemType Directory -Force -Path (Split-Path $exePath) | Out-Null
        for ($try = 1; ; $try++) {
            try { Copy-Item -LiteralPath $source -Destination $exePath -Force; break }
            catch { if ($try -ge 10) { throw }; Start-Sleep -Milliseconds 500 }
        }
    }
    if (-not (Test-Path -LiteralPath $exePath)) { throw "$exePath does not exist. Build peek first." }

    Remove-PeekFirewallRules
    # peek itself refuses anything but these ranges; the firewall rule matches,
    # so it is safe on a network Windows has classed as Public too. Without a
    # token peek accepts Tailscale only, and so does the rule.
    $noToken = $PeekArgs -match '(^|\s)--no-token(\s|$)'
    $remote = if ($noToken) { @('100.64.0.0/10', 'fd7a:115c:a1e0::/48') }
              else { @('LocalSubnet', '10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '100.64.0.0/10', '169.254.0.0/16') }
    New-NetFirewallRule -DisplayName $name -Description 'peek remote desktop host' `
        -Direction Inbound -Action Allow -Program $exePath -Protocol TCP -Profile Any `
        -RemoteAddress $remote |
        Out-Null

    $arguments = ("--detach --log `"$log`" $PeekArgs").Trim()
    $action = New-ScheduledTaskAction -Execute $exePath -Argument $arguments
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
    $principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Principal $principal `
        -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $name

    Say "Installed the '$name' firewall rule and logon task for $user, and started peek."
    Say "  $exePath $arguments"
    if ($noToken) {
        Say "No access token: open http://<this machine's Tailscale IP or name>:<port>/ on any device on your tailnet."
    } else {
        Say "Pair a browser with ``peek pair``. The access token is in $(Join-Path (Split-Path $log) 'token')."
    }
} catch {
    Say "Error: $($_.Exception.Message)"
    exit 1
}
