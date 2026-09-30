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
