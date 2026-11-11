<#
Sets peek up as a background service for this user. It:

  - allows peek.exe through Windows Firewall, from private and Tailscale
    addresses only;
  - registers a logon task that starts peek hidden and as administrator.

Running as administrator is what lets it control windows that are themselves
elevated (Task Manager, installers, admin terminals). The scheduled task is
allowed to start elevated without a UAC prompt each time.

Run from an elevated PowerShell:
    .\scripts\install-autostart.ps1              # install and start now
    .\scripts\install-autostart.ps1 -Remove      # uninstall
    .\scripts\install-autostart.ps1 -PeekArgs '--port 9000 --no-clipboard'
#>
#Requires -RunAsAdministrator
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\peek.exe'),
    [string]$PeekArgs = '',
    [switch]$Remove
)
$ErrorActionPreference = 'Stop'
$name = 'peek'
$exePath = (Resolve-Path $Exe).Path

# Windows adds "block" rules of its own the first time a program listens and
# nobody answers the prompt; those would override an allow rule, so clear
# everything that refers to this executable first.
function Remove-PeekFirewallRules {
    Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue |
        Where-Object { $_.Program -eq $exePath } |
        Get-NetFirewallRule | Remove-NetFirewallRule
}

if ($Remove) {
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
    Get-Process -Name peek -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-PeekFirewallRules
    "Removed the '$name' logon task and firewall rule, and stopped peek."
    return
}

Remove-PeekFirewallRules
# peek itself refuses anything but these ranges; the firewall rule matches,
# so it is safe on a network Windows has classed as Public too.
New-NetFirewallRule -DisplayName $name -Description 'peek remote desktop host' `
    -Direction Inbound -Action Allow -Program $exePath -Protocol TCP -Profile Any `
    -RemoteAddress @('LocalSubnet', '10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '100.64.0.0/10', '169.254.0.0/16') |
    Out-Null

$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$log = Join-Path $env:APPDATA 'peek\peek.log'
$arguments = ("--detach --log `"$log`" $PeekArgs").Trim()

$action = New-ScheduledTaskAction -Execute $exePath -Argument $arguments
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero)
Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Principal $principal `
    -Settings $settings -Force | Out-Null

# Replace any copy that is already running, then start the new one.
Get-Process -Name peek -ErrorAction SilentlyContinue | Stop-Process -Force
Start-ScheduledTask -TaskName $name
"Installed the '$name' firewall rule and logon task for $user, and started peek."
"  $exePath $arguments"
"The access URL is printed when peek is run in a terminal; the token is in $(Join-Path $env:APPDATA 'peek\token')."
