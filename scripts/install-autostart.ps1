<#
Sets peek up as a background service. It:

  - allows peek.exe through Windows Firewall, from private and Tailscale
    addresses only;
  - registers a logon task that starts peek hidden and as administrator, or
    with -Service, a Windows service that starts peek as SYSTEM in the
    session at the console. Installing one removes the other.

Running as administrator is what lets it control windows that are themselves
elevated (Task Manager, installers, admin terminals). The scheduled task is
allowed to start elevated without a UAC prompt each time.

As SYSTEM, the service's peek also shows and controls the login screen, the
lock screen and UAC prompts, and keeps running while nobody is signed in. It
runs a copy under Program Files, which only administrators can replace, and
keeps its token and paired browsers in %ProgramData%\peek, which only
administrators and the user who installed it can read. Switching between the
two carries the token and paired browsers over.

`peek install` runs this script for you (with -Config). By hand, from an
elevated PowerShell in a source checkout:
    .\scripts\install-autostart.ps1              # install and start now
    .\scripts\install-autostart.ps1 -Service     # the same, as a service
    .\scripts\install-autostart.ps1 -Remove      # uninstall either
    .\scripts\install-autostart.ps1 -PeekArgs '--port 9000 --no-clipboard'
    .\scripts\install-autostart.ps1 -PeekArgs '--no-token'   # Tailscale only, no token
#>
#Requires -RunAsAdministrator
param(
    [string]$Exe = (Join-Path $PSScriptRoot '..\target\release\peek.exe'),
    [string]$PeekArgs = '',
    [switch]$Service,
    [switch]$Remove,
    # A JSON file from `peek install` / `peek uninstall`. It names the user
    # and paths explicitly: the elevated script may run as a different account
    # (a standard user types an administrator's password at the UAC prompt).
    [string]$Config
)
$ErrorActionPreference = 'Stop'
$name = 'peek'
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$userConfig = Join-Path $env:APPDATA 'peek'
$machineConfig = Join-Path $env:ProgramData 'peek'
# The logon task's copy of peek, when `peek install` made one.
$userExe = $null
$serviceExe = Join-Path $env:ProgramFiles 'peek\peek.exe'
$source = $null
$callerPid = 0
$output = $null

if ($Config) {
    $c = Get-Content -Raw -LiteralPath $Config | ConvertFrom-Json
    $Remove = [bool]$c.remove
    $Service = [bool]$c.service
    $callerPid = [int]$c.callerPid
    $output = $c.output
    $userExe = $c.userExe
    $serviceExe = $c.serviceExe
    $userConfig = $c.userConfig
    $machineConfig = $c.machineConfig
    if ($c.source) { $source = $c.source }
    if ($c.user) { $user = $c.user }
    $Exe = if ($Service) { $serviceExe } else { $userExe }
    # Quote each flag that needs it, for the task's command line.
    $PeekArgs = (@($c.peekArgs) | Where-Object { $_ -ne $null } | ForEach-Object {
        if ($_ -match '[\s"]') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ }
    }) -join ' '
} elseif ($Service) {
    # SYSTEM will run it: copy it where only administrators can replace it.
    $source = $Exe
    $Exe = $serviceExe
}
$exePath = [System.IO.Path]::GetFullPath($Exe)
$log = if ($Service) { Join-Path $machineConfig 'peek.log' } else { Join-Path $userConfig 'peek.log' }

# Messages go to the console, and to a file when `peek install` asked for one
# (an elevated window cannot write to the console that started it).
function Say([string]$line) {
    $line
    if ($output) { Add-Content -LiteralPath $output -Value $line -Encoding UTF8 }
}

# Windows adds "block" rules of its own the first time a program listens and
# nobody answers the prompt; those would override an allow rule, so clear
# everything that refers to either installed copy first, and earlier peek
# rules for another location.
function Remove-PeekFirewallRules {
    $programs = @($exePath, $userExe, $serviceExe) | Where-Object { $_ } |
        ForEach-Object { [System.IO.Path]::GetFullPath($_) }
    Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue |
        Where-Object { $programs -contains $_.Program } |
        Get-NetFirewallRule | Remove-NetFirewallRule
    Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}

# Every running peek except the `peek install` that started this script.
function Stop-Peek {
    $running = @(Get-Process -Name peek -ErrorAction SilentlyContinue | Where-Object { $_.Id -ne $callerPid })
    # (One that will not stop shows up below, when its file cannot be replaced.)
    $running | Stop-Process -Force -ErrorAction SilentlyContinue
    $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

# Stopping the service also ends the peek it started.
function Stop-PeekService {
    $svc = Get-Service -Name $name -ErrorAction SilentlyContinue
    if (-not $svc) { return }
    if ($svc.Status -ne 'Stopped') {
        Stop-Service -Name $name -Force -ErrorAction SilentlyContinue
        try { $svc.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(15)) } catch {}
    }
    $svc.Close()
}

function Remove-PeekService {
    if (-not (Get-Service -Name $name -ErrorAction SilentlyContinue)) { return }
    Stop-PeekService
    sc.exe delete $name | Out-Null
}

function Remove-PeekTask {
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction SilentlyContinue
}

function Remove-Copy([string]$path) {
    if ($path -and (Test-Path -LiteralPath $path)) {
        Remove-Item -LiteralPath $path -Force
        Remove-Item -LiteralPath (Split-Path $path) -ErrorAction SilentlyContinue
    }
}

# The token, this machine's id and the paired browsers, so that switching
# between the logon task and the service keeps bookmarks and paired browsers
# working. (Each keeps its own local-secret.)
function Copy-Identity([string]$from, [string]$to) {
    foreach ($file in 'token', 'host-id', 'clients.json') {
        $path = Join-Path $from $file
        if (Test-Path -LiteralPath $path) {
            New-Item -ItemType Directory -Force -Path $to | Out-Null
            Copy-Item -LiteralPath $path -Destination (Join-Path $to $file) -Force
        }
    }
}

# The service's settings control this PC, at the login screen too: only
# SYSTEM and administrators may change them, and only they and the user who
# installed it may read them (so that `peek pair` works for that user without
# administrator rights). Anything already inside takes the same rights.
function Protect-MachineConfig {
    New-Item -ItemType Directory -Force -Path $machineConfig | Out-Null
    $sid = (New-Object System.Security.Principal.NTAccount($user)).Translate(
        [System.Security.Principal.SecurityIdentifier]).Value
    $acl = Get-Acl -LiteralPath $machineConfig
    # Full control for SYSTEM and Administrators, read for the user.
    $acl.SetSecurityDescriptorSddlForm("D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;$sid)", 'Access')
    Set-Acl -LiteralPath $machineConfig -AclObject $acl
}

try {
    $hadService = [bool](Get-Service -Name $name -ErrorAction SilentlyContinue)

    if ($Remove) {
        Remove-PeekService
        Remove-PeekTask
        Stop-Peek
        Remove-PeekFirewallRules
        Remove-Copy $serviceExe
        if ($Config) { Remove-Copy $userExe }
        $what = if ($hadService) { 'service' } else { 'logon task' }
        Say "Removed the '$name' $what and firewall rule, and stopped peek."
        return
    }

    # Replace the installed copy: stop whatever runs it first, since Windows
    # will not overwrite a running executable (and two peeks would want the
    # same port).
    Stop-PeekService
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
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

    if ($Service) {
        Protect-MachineConfig
        if (-not $hadService) { Copy-Identity $userConfig $machineConfig }
        Remove-PeekTask
        Remove-Copy $userExe

        $binPath = ("`"$exePath`" --service --log `"$log`" $PeekArgs").Trim()
        $existing = Get-CimInstance -ClassName Win32_Service -Filter "Name='$name'"
        if ($existing) {
            $result = $existing | Invoke-CimMethod -MethodName Change -Arguments @{ PathName = $binPath; StartMode = 'Automatic' }
            if ($result.ReturnValue -ne 0) { throw "could not update the '$name' service (error $($result.ReturnValue))" }
        } else {
            New-Service -Name $name -BinaryPathName $binPath -DisplayName $name -StartupType Automatic `
                -Description 'peek remote desktop host: runs peek as SYSTEM in the session at the console' |
                Out-Null
        }
        # Start it again if it ever stops by itself.
        sc.exe failure $name reset= 86400 actions= restart/5000/restart/5000/restart/60000 | Out-Null
        Start-Service -Name $name

        Say "Installed the '$name' firewall rule and service, and started it."
        Say "  $binPath"
    } else {
        if ($hadService) {
            Remove-PeekService
            Copy-Identity $machineConfig $userConfig
            Remove-Copy $serviceExe
        }

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
    }
    if ($noToken) {
        Say "No access token: open http://<this machine's Tailscale IP or name>:<port>/ on any device on your tailnet."
    } else {
        Say "Pair a browser with ``peek pair``. The access token is in $(Join-Path (Split-Path $log) 'token')."
    }
} catch {
    Say "Error: $($_.Exception.Message)"
    exit 1
}
