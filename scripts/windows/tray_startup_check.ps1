#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Run tray recovery after the existing AAG login startup tasks finish.
.DESCRIPTION
    Register this script as a delayed interactive-logon task. It waits for AAG
    startup and the local gateway listener before running the cache reset,
    then checks Explorer and the gateway again. It does not start a gateway.
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 300)]
    [int]$startup_timeout_seconds = 120,
    [switch]$check_only
)

$ErrorActionPreference = 'Stop'
$startup_task_names = @('AAG Gateway Autostart', 'AAG Codex Mirror Sync')
$poll_interval_seconds = 2
$gateway_port = 8989
$repository_root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$gateway_executable = Join-Path $repository_root 'src-tauri\target\release\ai-agent-gateway.exe'
$recovery_script = Join-Path $PSScriptRoot 'reset_tray_icon_cache.ps1'
$powershell_executable = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
$explorer_executable = Join-Path $env:WINDIR 'explorer.exe'
$session_id = (Get-Process -Id $PID).SessionId

function assert_desktop_health {
    $explorer_processes = @(Get-Process -Name explorer -ErrorAction SilentlyContinue |
        Where-Object { $_.SessionId -eq $session_id })
    if ($explorer_processes.Count -eq 0) {
        throw 'Explorer is not running in this interactive session.'
    }
    $gateway_listener = @(Get-NetTCPConnection -LocalPort $gateway_port -State Listen |
        Where-Object { $_.LocalAddress -eq '127.0.0.1' })
    if ($gateway_listener.Count -ne 1) {
        throw 'Expected exactly one local AAG gateway listener.'
    }
    $gateway_process = Get-Process -Id $gateway_listener[0].OwningProcess
    if ($gateway_process.Path -ne $gateway_executable -or $gateway_process.SessionId -ne $session_id) {
        throw 'The gateway listener does not belong to the expected AAG executable and session.'
    }
}

function wait_for_startup {
    $deadline = (Get-Date).AddSeconds($startup_timeout_seconds)
    do {
        try {
            foreach ($task_name in $startup_task_names) {
                $startup_task = Get-ScheduledTask -TaskPath '\' -TaskName $task_name
                if ($startup_task.State -ne 'Ready') {
                    throw "Startup task is not finished and enabled: $task_name"
                }
            }
            assert_desktop_health
            return
        }
        catch {
            if ((Get-Date) -ge $deadline) {
                throw
            }
            Start-Sleep -Seconds $poll_interval_seconds
        }
    } while ($true)
}

try {
    wait_for_startup
    # The legacy reset stops Explorer by name. Refuse to affect another session.
    $other_session_explorers = @(Get-Process -Name explorer -ErrorAction SilentlyContinue |
        Where-Object { $_.SessionId -ne $session_id })
    if ($other_session_explorers.Count -gt 0) {
        throw 'Tray recovery skipped because another session is running Explorer.'
    }
    foreach ($task_name in $startup_task_names) {
        $startup_info = Get-ScheduledTaskInfo -TaskPath '\' -TaskName $task_name
        if ($startup_info.LastTaskResult -ne 0) {
            Write-Warning "Earlier startup task returned $($startup_info.LastTaskResult): $task_name. Live listener ownership was checked separately."
        }
    }
    if (-not $check_only) {
        try {
            & $powershell_executable -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $recovery_script
            if ($LASTEXITCODE -ne 0) {
                throw "Tray recovery failed with exit code $LASTEXITCODE."
            }
        }
        finally {
            $explorer_processes = @(Get-Process -Name explorer -ErrorAction SilentlyContinue |
                Where-Object { $_.SessionId -eq $session_id })
            if ($explorer_processes.Count -eq 0) {
                Start-Process -FilePath $explorer_executable -WindowStyle Hidden
            }
        }
        wait_for_startup
    }
    Write-Output 'Tray startup check passed: Explorer and the expected AAG listener are available.'
    exit 0
}
catch {
    Write-Error -ErrorRecord $_ -ErrorAction Continue
    exit 1
}
