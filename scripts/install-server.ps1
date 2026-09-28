# install-server.ps1 [-Exe <xi-vault.exe>] [-Seed <site.zip>] [-Share]: a Windows machine (a VM at home, say) serves game
# versions to players' launchers. Double-click install-server.cmd beside it (it asks for administrator
# rights itself); or in PowerShell: powershell -ExecutionPolicy Bypass -File .\install-server.ps1
# The program (ffxi-update-publisher.exe or xi-vault.exe) and a site-seed-*.zip are found beside it.
# It puts xi-vault in C:\xi-vault (the update publisher is the same program), the site in
# C:\xi-vault\site, a task that serves it on port 54080 from startup (as SYSTEM, restarted if it
# stops), and a firewall rule for the port. -Share also shares the site as \\<this PC>\xi-vault-site
# (read and write for you), so another PC can publish into it. -Seed unzips a site into it first (a
# version's manifest published --since itself: what the server's players have, so an update
# published here hosts only what it adds).
#
# Then: forward TCP 54080 on the router to this machine, point update.<your server> at your home
# address (DNS), and publish a version: double-click the update publisher on the PC with the game
# and give it the site folder (vault/README.md).
param(
    [string]$Exe = "",
    [int]$Port = 54080,
    [string]$Seed = "",
    [switch]$Share
)
$ErrorActionPreference = "Stop"
Set-Location -LiteralPath $PSScriptRoot
# beside this script when not given: the program (either name), and a seed when there is one
if (-not $Exe) { $Exe = @("ffxi-update-publisher.exe", "xi-vault.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1 }
if (-not $Seed) { $Seed = Get-ChildItem -Filter "site-seed-*.zip" -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty FullName }
if (-not $Exe -or -not (Test-Path $Exe)) { throw "No xi-vault program beside this script (give it with -Exe)." }
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    # again as an administrator (Windows asks), in a window that stays open to show the result
    $again = @("-NoExit", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"", "-Exe", "`"$((Resolve-Path $Exe).Path)`"", "-Port", $Port)
    if ($Seed) { $again += @("-Seed", "`"$((Resolve-Path $Seed).Path)`"") }
    if ($Share) { $again += "-Share" }
    Start-Process powershell -Verb RunAs -ArgumentList $again
    return
}

$root = "C:\xi-vault"
$site = "$root\site"
New-Item -ItemType Directory -Force -Path $site | Out-Null
Get-ScheduledTask -TaskName "xi-vault" -ErrorAction SilentlyContinue | Stop-ScheduledTask
Get-Process -Name "xi-vault" -ErrorAction SilentlyContinue | Stop-Process -Force
Copy-Item -Force $Exe "$root\xi-vault.exe"
# a seed starts a new site only: never over the versions a site publishes already
if ($Seed -and -not (Test-Path "$site\index.json")) {
    if (-not (Test-Path $Seed)) { throw "No seed at $Seed." }
    Expand-Archive -Force -Path $Seed -DestinationPath $site
}
if (-not (Test-Path "$site\index.json")) {
    Set-Content -Encoding ascii -Path "$site\index.json" -Value '{"format":"xi-vault/1","current":"","versions":[],"packs":[]}'
}

# served from startup, whoever is signed in
$action = New-ScheduledTaskAction -Execute "$root\xi-vault.exe" -Argument "serve `"$site`" --listen 0.0.0.0:$Port" -WorkingDirectory $root
$trigger = New-ScheduledTaskTrigger -AtStartup
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
$principal = New-ScheduledTaskPrincipal -UserId "SYSTEM" -LogonType ServiceAccount -RunLevel Highest
Register-ScheduledTask -TaskName "xi-vault" -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null
Start-ScheduledTask -TaskName "xi-vault"

Get-NetFirewallRule -DisplayName "xi-vault" -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -DisplayName "xi-vault" -Direction Inbound -Protocol TCP -LocalPort $Port -Action Allow | Out-Null

if ($Share) {
    if (-not (Get-SmbShare -Name "xi-vault-site" -ErrorAction SilentlyContinue)) {
        New-SmbShare -Name "xi-vault-site" -Path $site -ChangeAccess "$env:USERDOMAIN\$env:USERNAME" | Out-Null
    }
    $acl = Get-Acl $site
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule("$env:USERDOMAIN\$env:USERNAME", "Modify", "ContainerInherit,ObjectInherit", "None", "Allow")))
    Set-Acl $site $acl
    Write-Host "Shared as \\$env:COMPUTERNAME\xi-vault-site"
}

Start-Sleep -Seconds 2
try {
    $index = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/index.json" -TimeoutSec 5
    Write-Host "xi-vault serves $site on port $Port (it hands out: $(if ($index.current) { $index.current } else { 'nothing yet' }))."
} catch {
    throw "xi-vault did not answer on port ${Port}: $_"
}
Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike "127.*" -and $_.IPAddress -notlike "169.254.*" } |
    ForEach-Object { Write-Host "  this machine: http://$($_.IPAddress):$Port/index.json" }
