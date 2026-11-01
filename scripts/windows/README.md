# `scripts/windows/` — Windows maintenance utilities

Small standalone PowerShell helpers for Windows-specific desktop upkeep.
These are not part of the npm build pipeline; run them directly from an
administrator PowerShell prompt when needed.

## Scripts

- [`reset_tray_icon_cache.ps1`](reset_tray_icon_cache.ps1) — Resets the
  notification-area (system tray) icon cache. Use this when Windows updates
  cause application tray icons to disappear or get stuck in the hidden
  overflow area. The script backs up the registry, clears
  `IconStreams` / `PastIconsStream`, removes Explorer icon-cache files,
  and restarts Explorer. No application data is lost.

  ```powershell
  .\scripts\windows\reset_tray_icon_cache.ps1
  ```

- [`tray_startup_check.ps1`](tray_startup_check.ps1) — Runs the cache reset as
  a delayed login startup check. It waits up to 120 seconds for the existing
  `AAG Gateway Autostart` and `AAG Codex Mirror Sync` tasks to finish
  and for Explorer and the expected local gateway listener to be available. After
  recovery it checks them again and returns a failing exit code on error.
  `-check_only` verifies readiness without resetting the cache.
  Earlier task failures are reported as warnings; they do not prevent tray
  repair when the running executable, user session, and listener ownership
  are correct. This script does not repair or certify those earlier tasks.

  Configure the separate `AAG_Tray_Final_Self_Check` scheduled task for the
  current interactive user, highest privileges, a two-minute logon delay,
  hidden PowerShell, `IgnoreNew` instance policy, and a six-minute execution
  limit. Keep the existing gateway task at limited privileges. Invoke this
  script with `-NoProfile -NonInteractive -WindowStyle Hidden
  -ExecutionPolicy Bypass -File`.
  No password or system-wide service is required.

  This resets the icon cache once per login, including when the cache was
  healthy. Explorer restarts and the taskbar may briefly disappear. Registry
  backups remain in the temporary directory printed by the reset script.
  Recovery refuses to run while another user session has Explorer open,
  because the legacy reset helper stops Explorer processes by name.
  The delay and task checks order recovery after AAG startup; Windows does
  not provide a global completion barrier for every application's login item.
  A successful result checks process and listener ownership, not upstream
  health, authenticated API behavior, or every application's icon appearance.
