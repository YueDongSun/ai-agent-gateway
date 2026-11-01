param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("preflight", "inspect", "close", "tray_show", "tray_hide", "tray_quit")]
    [string]$Action,
    [int]$ProcessId,
    [string]$ExecutablePath,
    [string]$StartTicks
)

$ErrorActionPreference = "Stop"

function Read-StartupValue([string]$Path) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($Path, $false)
    if ($null -eq $key) { return @{ exists = $false } }
    try {
        if ($key.GetValueNames() -notcontains "AI Agent Gateway") { return @{ exists = $false } }
        $kind = $key.GetValueKind("AI Agent Gateway").ToString()
        $value = $key.GetValue("AI Agent Gateway", $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($value -is [byte[]]) { $value = [Convert]::ToBase64String($value) }
        return [ordered]@{ exists = $true; kind = $kind; value = $value }
    } finally { $key.Dispose() }
}

if ($Action -eq "preflight") {
    $singleton = $null
    try {
        $single_instance_present = [Threading.Mutex]::TryOpenExisting("org.aagateway.desktop-sim", [ref]$singleton)
    } finally { if ($null -ne $singleton) { $singleton.Dispose() } }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $app_data = Join-Path ([Environment]::GetFolderPath("LocalApplicationData")) "org.aagateway.desktop"
    $root_exists = Test-Path -LiteralPath $app_data -PathType Container
    $root_entries = @(if ($root_exists) {
        Get-ChildItem -LiteralPath $app_data -Force -ErrorAction Stop | Sort-Object Name | ForEach-Object {
            if ($_.PSIsContainer) { [ordered]@{ name = $_.Name; directory = $true } }
            else { [ordered]@{ name = $_.Name; directory = $false; size = $_.Length; written = $_.LastWriteTimeUtc.Ticks.ToString() } }
        }
    })
    [pscustomobject]@{
        elevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        running_count = @(Get-Process -Name "ai-agent-gateway" -ErrorAction SilentlyContinue).Count
        single_instance_present = $single_instance_present
        autostart_registration = [ordered]@{
            run = Read-StartupValue "Software\Microsoft\Windows\CurrentVersion\Run"
            approved = Read-StartupValue "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"
        }
        live_directory = [ordered]@{ root_exists = $root_exists; entries = $root_entries }
    } | ConvertTo-Json -Compress -Depth 6
    exit 0
}

if ($ProcessId -le 0 -or -not $ExecutablePath) { throw "desktop_native_target_required" }
$target = Get-Process -Id $ProcessId -ErrorAction Stop
$expected_path = [IO.Path]::GetFullPath($ExecutablePath)
if ([IO.Path]::GetFileName($expected_path) -ine "ai-agent-gateway.exe") { throw "desktop_native_target_binary_invalid" }
if (-not [string]::Equals($target.Path, $expected_path, [StringComparison]::OrdinalIgnoreCase)) {
    throw "desktop_native_target_path_changed"
}
$actual_start_ticks = $target.StartTime.ToUniversalTime().Ticks.ToString()
if (($Action -ne "inspect" -or $StartTicks) -and $StartTicks -ne $actual_start_ticks) { throw "desktop_native_target_identity_changed" }

Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class AagDesktopNative {
    private delegate bool EnumWindowsProc(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr window, out uint process_id);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr window, StringBuilder name, int length);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr window, StringBuilder text, int length);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)] private static extern bool PostMessage(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", SetLastError = true)] private static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
    [DllImport("user32.dll")] private static extern int GetMenuItemCount(IntPtr menu);
    [DllImport("user32.dll")] private static extern uint GetMenuItemID(IntPtr menu, int position);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetMenuString(IntPtr menu, uint position, StringBuilder text, int length, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError = true)] private static extern bool AttachConsole(uint process_id);

    public static IntPtr Find(int process_id, string class_name, string title) {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate(IntPtr window, IntPtr parameter) {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner != (uint)process_id) return true;
            var value = new StringBuilder(256);
            if (!string.IsNullOrEmpty(class_name)) {
                GetClassName(window, value, value.Capacity);
                if (value.ToString() != class_name) return true;
            }
            if (!string.IsNullOrEmpty(title)) {
                value.Clear();
                GetWindowText(window, value, value.Capacity);
                if (value.ToString() != title) return true;
            }
            found = window;
            return false;
        }, IntPtr.Zero);
        return found;
    }

    public static string[] WindowClasses(int process_id) {
        var classes = new List<string>();
        EnumWindows(delegate(IntPtr window, IntPtr parameter) {
            uint owner;
            GetWindowThreadProcessId(window, out owner);
            if (owner == (uint)process_id) {
                var value = new StringBuilder(256);
                GetClassName(window, value, value.Capacity);
                classes.Add(value.ToString());
            }
            return true;
        }, IntPtr.Zero);
        return classes.ToArray();
    }

    public static bool HasConsole(int process_id) {
        FreeConsole();
        if (AttachConsole((uint)process_id)) { FreeConsole(); return true; }
        if (Marshal.GetLastWin32Error() != 6) throw new InvalidOperationException("desktop_native_console_inspection_failed");
        return false;
    }

    public static void Deliver(IntPtr window, uint message, uint wparam, int lparam) {
        if (window == IntPtr.Zero || !PostMessage(window, message, new UIntPtr(wparam), new IntPtr(lparam))) {
            throw new InvalidOperationException("desktop_native_window_message_failed");
        }
    }

    public static void TrayMenu(int process_id, string text) {
        IntPtr tray = Find(process_id, "tray_icon_app", null);
        // The callback number belongs to the locked tray-icon Windows implementation.
        Deliver(tray, 6002, 0, 0x205);
        var deadline = Stopwatch.StartNew();
        while (deadline.ElapsedMilliseconds < 5000) {
            IntPtr popup = Find(process_id, "#32768", null);
            UIntPtr result;
            if (popup != IntPtr.Zero && SendMessageTimeout(popup, 0x1e1, UIntPtr.Zero, IntPtr.Zero, 2, 500, out result) != IntPtr.Zero) {
                IntPtr menu = new IntPtr(unchecked((long)result.ToUInt64()));
                for (int position = 0; position < GetMenuItemCount(menu); position++) {
                    var label = new StringBuilder(256);
                    GetMenuString(menu, (uint)position, label, label.Capacity, 0x400);
                    if (label.ToString().Replace("&", "") != text) continue;
                    uint id = GetMenuItemID(menu, position);
                    if (id == 0 || id == uint.MaxValue) throw new InvalidOperationException("desktop_native_menu_id_invalid");
                    Deliver(tray, 0x1f, 0, 0);
                    Deliver(tray, 0x111, id, 0);
                    return;
                }
            }
            Thread.Sleep(50);
        }
        Deliver(tray, 0x1f, 0, 0);
        throw new InvalidOperationException("desktop_native_tray_menu_unavailable");
    }
}
"@

$current_target = Get-Process -Id $ProcessId -ErrorAction Stop
if ($current_target.StartTime.ToUniversalTime().Ticks.ToString() -ne $actual_start_ticks -or
    -not [string]::Equals($current_target.Path, $expected_path, [StringComparison]::OrdinalIgnoreCase)) {
    throw "desktop_native_target_identity_changed"
}
$main = [AagDesktopNative]::Find($ProcessId, $null, "AI Agent Gateway")
$tray = [AagDesktopNative]::Find($ProcessId, "tray_icon_app", $null)
switch ($Action) {
    "inspect" {
        [pscustomobject]@{
            start_ticks = $actual_start_ticks
            main_window = ($main -ne [IntPtr]::Zero)
            visible = ($main -ne [IntPtr]::Zero -and [AagDesktopNative]::IsWindowVisible($main))
            tray_window = ($tray -ne [IntPtr]::Zero)
            window_classes = @([AagDesktopNative]::WindowClasses($ProcessId))
            has_console = [AagDesktopNative]::HasConsole($ProcessId)
        } | ConvertTo-Json -Compress
    }
    "close" { [AagDesktopNative]::Deliver($main, 0x10, 0, 0) }
    "tray_show" { [AagDesktopNative]::Deliver($tray, 6002, 0, 0x202) }
    "tray_hide" { [AagDesktopNative]::TrayMenu($ProcessId, "Hide") }
    "tray_quit" { [AagDesktopNative]::TrayMenu($ProcessId, "Quit") }
}
