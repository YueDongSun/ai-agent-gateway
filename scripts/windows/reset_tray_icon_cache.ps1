#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Reset the Windows notification-area (system tray) icon cache.

.DESCRIPTION
    Windows updates, especially Insider builds, can corrupt the tray icon
    history stored in IconStreams / PastIconsStream. This leaves many apps
    hidden in the overflow area or unable to show a tray icon at all.

    The script backs up the relevant registry key, removes the cached icon
    history, clears Explorer icon-cache files, and restarts Explorer.
    No application data is lost.

.EXAMPLE
    .\reset_tray_icon_cache.ps1
#>

[CmdletBinding(SupportsShouldProcess)]
param()

$ErrorActionPreference = "Stop"

$tray_key = "HKCU:\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\TrayNotify"
$backup_dir = "$env:TEMP\aag_tray_backup_$(Get-Date -Format 'yyyyMMdd_HHmmss')"
$explorer_cache_dir = "$env:LOCALAPPDATA\Microsoft\Windows\Explorer"

function Test-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-Admin)) {
    Write-Error "This script must run as Administrator. Right-click PowerShell and choose 'Run as administrator'."
    exit 1
}

if (-not (Test-Path $tray_key)) {
    Write-Error "Tray notify registry key not found: $tray_key"
    exit 1
}

New-Item -ItemType Directory -Path $backup_dir -Force | Out-Null
$backup_file = Join-Path $backup_dir "TrayNotify.reg"

Write-Host "Backing up registry to $backup_file ..."
$reg_path = $tray_key -replace '^HKCU:\\', 'HKEY_CURRENT_USER\'
$reg_export = Start-Process -FilePath "reg.exe" -ArgumentList "export", "`"$reg_path`"", "`"$backup_file`"", "/y" -Wait -PassThru -NoNewWindow
if ($reg_export.ExitCode -ne 0) {
    Write-Error "Failed to backup registry. Exit code: $($reg_export.ExitCode)"
    exit 1
}

Write-Host "Stopping Windows Explorer ..."
Stop-Process -Name "explorer" -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

Write-Host "Clearing tray icon cache entries ..."
"IconStreams", "PastIconsStream" | ForEach-Object {
    if (Get-ItemProperty -Path $tray_key -Name $_ -ErrorAction SilentlyContinue) {
        Remove-ItemProperty -Path $tray_key -Name $_ -Force
        Write-Host "  Removed $_"
    }
    else {
        Write-Host "  $_ not present (nothing to clear)"
    }
}

Write-Host "Clearing Explorer icon cache files ..."
$icon_cache_files = Get-ChildItem -Path $explorer_cache_dir -Name "iconcache*" -ErrorAction SilentlyContinue
if ($icon_cache_files) {
    $icon_cache_files | ForEach-Object {
        $full = Join-Path $explorer_cache_dir $_
        Remove-Item -Path $full -Force -ErrorAction SilentlyContinue
        Write-Host "  Removed $_"
    }
}
else {
    Write-Host "  No iconcache files found"
}

Write-Host "Restarting Windows Explorer ..."
Start-Process "explorer"

Write-Host ""
Write-Host "Done. If some icons are still missing, enable them manually:"
Write-Host "  Settings > Personalization > Taskbar > Other system tray icons"
Write-Host ""
Write-Host "Registry backup kept at: $backup_file"
