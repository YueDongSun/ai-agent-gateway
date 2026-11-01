$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

$native_code = @'
using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace Aag.RustQualification
{
    [StructLayout(LayoutKind.Sequential)]
    public struct Luid
    {
        public uint LowPart;
        public int HighPart;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct LuidAndAttributes
    {
        public Luid Luid;
        public uint Attributes;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct TokenPrivilegesHeader
    {
        public uint Count;
        public LuidAndAttributes FirstEntry;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct SidAndAttributes
    {
        public IntPtr Sid;
        public uint Attributes;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct TokenGroupsHeader
    {
        public uint Count;
        public SidAndAttributes FirstEntry;
    }

    public sealed class TokenProbe
    {
        public bool ProcessTokenAllAccess;
        public bool ThreadNotImpersonating;
        public bool PrimaryProcessToken;
        public bool TokenHasRestrictionsZero;
        public bool IsTokenRestrictedFalse;
        public bool GroupsQuerySucceeded;
        public int LogonSidCount;
        public int EnabledNonDenyLogonSidCount;
        public bool PrivilegesQuerySucceeded;
        public bool SeBackupAvailable;
        public bool SeBackupEnabled;
        public bool SeRestoreAvailable;
        public bool SeRestoreEnabled;
        public bool SeSecurityAvailable;
        public bool SeSecurityEnabled;
    }

    public static class Native
    {
        private const uint TokenAllAccess = 0x000F01FF;
        private const uint GroupLogonId = 0xC0000000;
        private const uint GroupEnabled = 0x00000004;
        private const uint GroupDenyOnly = 0x00000010;
        private const uint PrivilegeEnabled = 0x00000002;
        private const uint PrivilegeRemoved = 0x00000004;
        private const uint FileAttributeDirectory = 0x00000010;
        private const uint FileAttributeReparsePoint = 0x00000400;
        private const uint InvalidFileAttributes = 0xFFFFFFFF;
        private const uint SymbolicLinkFlagDirectory = 0x1;
        private const uint SymbolicLinkFlagAllowUnprivilegedCreate = 0x2;

        [DllImport("kernel32.dll")]
        private static extern IntPtr GetCurrentProcess();

        [DllImport("kernel32.dll")]
        private static extern IntPtr GetCurrentThread();

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool OpenProcessToken(IntPtr process, uint desiredAccess, out IntPtr token);

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool OpenThreadToken(IntPtr thread, uint desiredAccess, bool openAsSelf, out IntPtr token);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool LookupPrivilegeValue(string systemName, string name, out Luid luid);

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool GetTokenInformation(IntPtr token, int informationClass, IntPtr information, uint informationLength, out uint returnLength);

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool IsTokenRestricted(IntPtr token);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetSystemDirectory(StringBuilder buffer, uint size);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetFileAttributes(string path);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CreateSymbolicLink(string symbolicLinkName, string targetName, uint flags);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool RemoveDirectory(string path);

        private static byte[] TokenInformation(IntPtr token, int informationClass)
        {
            uint required;
            GetTokenInformation(token, informationClass, IntPtr.Zero, 0, out required);
            if (required == 0 || required > 1024 * 1024) throw new InvalidOperationException();
            IntPtr buffer = Marshal.AllocHGlobal((int)required);
            try
            {
                uint returned;
                if (!GetTokenInformation(token, informationClass, buffer, required, out returned)
                    || returned == 0 || returned > required) throw new InvalidOperationException();
                byte[] result = new byte[returned];
                Marshal.Copy(buffer, result, 0, result.Length);
                return result;
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static bool IsLogonSid(IntPtr sid)
        {
            if (sid == IntPtr.Zero) return false;
            if (Marshal.ReadByte(sid, 0) != 1) return false;
            int authority = 0;
            for (int index = 2; index < 8; index++) authority = (authority << 8) | Marshal.ReadByte(sid, index);
            if (authority != 5) return false;
            int subAuthorityCount = Marshal.ReadByte(sid, 1);
            // Production token.rs admits the S-1-5-5-X-Y logon SID shape:
            // only the first subauthority is fixed at 5; the second is the
            // variable logon id, and the trailing component must exist.
            if (subAuthorityCount < 2) return false;
            return Marshal.ReadInt32(sid, 8) == 5;
        }

        private static void ReadLogonSidCounts(IntPtr token, TokenProbe probe)
        {
            uint required;
            GetTokenInformation(token, 2, IntPtr.Zero, 0, out required);
            if (required == 0 || required > 1024 * 1024) throw new InvalidOperationException();
            // TOKEN_GROUPS entries embed SID pointers into this native buffer,
            // so every SID dereference must happen before the buffer is freed.
            IntPtr buffer = Marshal.AllocHGlobal((int)required);
            try
            {
                uint returned;
                if (!GetTokenInformation(token, 2, buffer, required, out returned)
                    || returned < 4 || returned > required) throw new InvalidOperationException();
                uint count = (uint)Marshal.ReadInt32(buffer, 0);
                int start = Marshal.OffsetOf(typeof(TokenGroupsHeader), "FirstEntry").ToInt32();
                int stride = Marshal.SizeOf(typeof(SidAndAttributes));
                if (count > 4096 || start + (long)count * stride > returned) throw new InvalidOperationException();
                for (int index = 0; index < count; index++)
                {
                    IntPtr entry = new IntPtr(buffer.ToInt64() + start + (long)index * stride);
                    IntPtr sid = Marshal.ReadIntPtr(entry, 0);
                    uint attributes = (uint)Marshal.ReadInt32(entry, IntPtr.Size);
                    if ((attributes & GroupLogonId) != GroupLogonId || !IsLogonSid(sid)) continue;
                    probe.LogonSidCount++;
                    if ((attributes & GroupEnabled) != 0 && (attributes & GroupDenyOnly) == 0)
                        probe.EnabledNonDenyLogonSidCount++;
                }
                probe.GroupsQuerySucceeded = true;
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static bool PrivilegeState(IntPtr token, string name, out bool enabled)
        {
            enabled = false;
            Luid wanted;
            if (!LookupPrivilegeValue(null, name, out wanted)) throw new InvalidOperationException();
            byte[] privileges = TokenInformation(token, 3);
            uint count = BitConverter.ToUInt32(privileges, 0);
            int start = Marshal.OffsetOf(typeof(TokenPrivilegesHeader), "FirstEntry").ToInt32();
            int stride = Marshal.SizeOf(typeof(LuidAndAttributes));
            if (count > 4096 || start + (long)count * stride > privileges.Length) throw new InvalidOperationException();
            for (int index = 0; index < count; index++)
            {
                int offset = start + index * stride;
                uint low = BitConverter.ToUInt32(privileges, offset);
                int high = BitConverter.ToInt32(privileges, offset + 4);
                uint attributes = BitConverter.ToUInt32(privileges, offset + 8);
                if (low != wanted.LowPart || high != wanted.HighPart) continue;
                if ((attributes & PrivilegeRemoved) != 0) return false;
                enabled = (attributes & PrivilegeEnabled) != 0;
                return true;
            }
            return false;
        }

        public static TokenProbe InspectProcessToken()
        {
            TokenProbe probe = new TokenProbe();
            IntPtr threadToken;
            if (OpenThreadToken(GetCurrentThread(), 0x0008, true, out threadToken))
            {
                CloseHandle(threadToken);
                probe.ThreadNotImpersonating = false;
            }
            else
            {
                probe.ThreadNotImpersonating = Marshal.GetLastWin32Error() == 1008;
            }

            IntPtr token;
            if (!OpenProcessToken(GetCurrentProcess(), TokenAllAccess, out token) || token == IntPtr.Zero)
                return probe;
            probe.ProcessTokenAllAccess = true;
            try
            {
                byte[] type = TokenInformation(token, 8);
                probe.PrimaryProcessToken = BitConverter.ToUInt32(type, 0) == 1;
                byte[] restrictions = TokenInformation(token, 21);
                // TokenHasRestrictions is a one-byte BOOL, unlike the four-byte enums.
                probe.TokenHasRestrictionsZero = restrictions.Length == 1 && restrictions[0] == 0;
                probe.IsTokenRestrictedFalse = !IsTokenRestricted(token);
                try { ReadLogonSidCounts(token, probe); } catch { probe.GroupsQuerySucceeded = false; }
                try
                {
                    probe.SeBackupAvailable = PrivilegeState(token, "SeBackupPrivilege", out probe.SeBackupEnabled);
                    probe.SeRestoreAvailable = PrivilegeState(token, "SeRestorePrivilege", out probe.SeRestoreEnabled);
                    probe.SeSecurityAvailable = PrivilegeState(token, "SeSecurityPrivilege", out probe.SeSecurityEnabled);
                    probe.PrivilegesQuerySucceeded = true;
                }
                catch
                {
                    probe.PrivilegesQuerySucceeded = false;
                }
            }
            catch
            {
                probe.PrimaryProcessToken = false;
                probe.TokenHasRestrictionsZero = false;
                probe.IsTokenRestrictedFalse = false;
            }
            finally
            {
                CloseHandle(token);
            }
            return probe;
        }

        private static string SystemRobocopyPath()
        {
            StringBuilder buffer = new StringBuilder(32768);
            uint length = GetSystemDirectory(buffer, (uint)buffer.Capacity);
            if (length == 0 || length >= buffer.Capacity) return null;
            string systemDirectory = buffer.ToString();
            if (!String.Equals(Path.GetFileName(systemDirectory), "System32", StringComparison.OrdinalIgnoreCase)) return null;
            string path = Path.Combine(systemDirectory, "robocopy.exe");
            string current = Path.GetDirectoryName(path);
            while (!String.IsNullOrEmpty(current))
            {
                uint attributes = GetFileAttributes(current);
                if (attributes == InvalidFileAttributes || (attributes & FileAttributeDirectory) == 0
                    || (attributes & FileAttributeReparsePoint) != 0) return null;
                DirectoryInfo parent = Directory.GetParent(current);
                current = parent == null ? null : parent.FullName;
            }
            uint fileAttributes = GetFileAttributes(path);
            if (fileAttributes == InvalidFileAttributes || (fileAttributes & FileAttributeDirectory) != 0
                || (fileAttributes & FileAttributeReparsePoint) != 0 || !File.Exists(path)) return null;
            string canonical = Path.GetFullPath(path);
            if (!String.Equals(canonical, path, StringComparison.OrdinalIgnoreCase)) return null;
            return path;
        }

        public static bool RobocopyHelpMatchesContract()
        {
            string path = SystemRobocopyPath();
            if (path == null) return false;
            try
            {
                StringBuilder output = new StringBuilder();
                int outputCharacters = 0;
                bool overLimit = false;
                ProcessStartInfo start = new ProcessStartInfo();
                start.FileName = path;
                start.Arguments = "/?";
                start.UseShellExecute = false;
                start.CreateNoWindow = true;
                start.RedirectStandardOutput = true;
                start.RedirectStandardError = true;
                start.WorkingDirectory = Path.GetTempPath();
                using (Process process = new Process())
                using (ManualResetEvent outputClosed = new ManualResetEvent(false))
                using (ManualResetEvent errorClosed = new ManualResetEvent(false))
                {
                    process.StartInfo = start;
                    process.OutputDataReceived += delegate(object sender, DataReceivedEventArgs args)
                    {
                        if (args.Data == null)
                        {
                            outputClosed.Set();
                            return;
                        }
                        outputCharacters += args.Data.Length + 1;
                        if (outputCharacters > 512 * 1024) overLimit = true;
                        else output.AppendLine(args.Data);
                    };
                    process.ErrorDataReceived += delegate(object sender, DataReceivedEventArgs args)
                    {
                        if (args.Data == null) errorClosed.Set();
                    };
                    if (!process.Start()) return false;
                    process.BeginOutputReadLine();
                    process.BeginErrorReadLine();
                    if (!process.WaitForExit(5000))
                    {
                        try { process.Kill(); } catch { }
                        process.WaitForExit(1000);
                        return false;
                    }
                    if (!outputClosed.WaitOne(2000) || !errorClosed.WaitOne(2000)) return false;
                    if (overLimit || (process.ExitCode != 0 && process.ExitCode != 16)) return false;
                }
                string ascii = output.ToString().Replace("\0", "").ToUpperInvariant();
                return ascii.Contains("ROBOCOPY") && ascii.Contains("/DCOPY:") && ascii.Contains("E=");
            }
            catch
            {
                return false;
            }
        }

        public static bool DirectorySymlinkWorksInTemporaryFixture()
        {
            string fixture = Path.Combine(Path.GetTempPath(), "aag_rust_qualification_" + Guid.NewGuid().ToString("N"));
            string target = Path.Combine(fixture, "target");
            string link = Path.Combine(fixture, "link");
            bool fixtureCreated = false;
            bool linkCreated = false;
            bool result = false;
            try
            {
                if (Directory.Exists(fixture) || File.Exists(fixture)) return false;
                Directory.CreateDirectory(fixture);
                fixtureCreated = true;
                Directory.CreateDirectory(target);
                linkCreated = CreateSymbolicLink(link, target,
                    SymbolicLinkFlagDirectory | SymbolicLinkFlagAllowUnprivilegedCreate);
                if (!linkCreated) return false;
                uint attributes = GetFileAttributes(link);
                result = attributes != InvalidFileAttributes
                    && (attributes & FileAttributeDirectory) != 0
                    && (attributes & FileAttributeReparsePoint) != 0;
            }
            catch
            {
                result = false;
            }
            finally
            {
                if (linkCreated)
                {
                    uint linkAttributes = GetFileAttributes(link);
                    if (linkAttributes != InvalidFileAttributes && (linkAttributes & FileAttributeReparsePoint) != 0)
                        RemoveDirectory(link);
                    else
                        result = false;
                }
                if (fixtureCreated)
                {
                    try
                    {
                        if (Directory.Exists(target)) Directory.Delete(target, false);
                        Directory.Delete(fixture, false);
                    }
                    catch
                    {
                        result = false;
                    }
                }
            }
            return result;
        }
    }
}
'@

$report = $null
try {
    Add-Type -TypeDefinition $native_code -Language CSharp
    $token = [Aag.RustQualification.Native]::InspectProcessToken()
    $robocopy_ok = [Aag.RustQualification.Native]::RobocopyHelpMatchesContract()
    $symlink_ok = [Aag.RustQualification.Native]::DirectorySymlinkWorksInTemporaryFixture()

    $codes = New-Object 'System.Collections.Generic.List[string]'
    if (-not $token.ProcessTokenAllAccess) { $codes.Add('process_token_all_access_unavailable') }
    if (-not $token.ThreadNotImpersonating) { $codes.Add('thread_impersonation_present_or_unproven') }
    if (-not $token.PrimaryProcessToken) { $codes.Add('original_primary_token_unproven') }
    if (-not $token.TokenHasRestrictionsZero) { $codes.Add('token_has_restrictions_nonzero_or_unavailable') }
    if (-not $token.IsTokenRestrictedFalse) { $codes.Add('token_is_restricted_or_unavailable') }
    if (-not $token.GroupsQuerySucceeded -or $token.LogonSidCount -ne 1 -or $token.EnabledNonDenyLogonSidCount -ne 1) {
        $codes.Add('enabled_logon_sid_requirement_unproven')
    }
    if (-not $token.PrivilegesQuerySucceeded) { $codes.Add('privilege_query_unavailable') }
    if (-not $token.SeBackupAvailable -or -not $token.SeRestoreAvailable -or -not $token.SeSecurityAvailable) {
        $codes.Add('required_privilege_unavailable')
    }
    if (-not $robocopy_ok) { $codes.Add('robocopy_contract_unproven') }
    if (-not $symlink_ok) { $codes.Add('directory_symlink_unavailable') }

    $report = [ordered]@{
        schema_version = 1
        status = $(if ($codes.Count -eq 0) { 'passed' } else { 'blocked' })
        evidence_class = 'local_prerequisites_only'
        process_token_all_access = [bool]$token.ProcessTokenAllAccess
        thread_not_impersonating = [bool]$token.ThreadNotImpersonating
        primary_process_token = [bool]$token.PrimaryProcessToken
        token_has_restrictions_zero = [bool]$token.TokenHasRestrictionsZero
        is_token_restricted_false = [bool]$token.IsTokenRestrictedFalse
        logon_sid_count = [int]$token.LogonSidCount
        enabled_non_deny_logon_sid_count = [int]$token.EnabledNonDenyLogonSidCount
        privileges_query_succeeded = [bool]$token.PrivilegesQuerySucceeded
        se_backup_available = [bool]$token.SeBackupAvailable
        se_backup_enabled = [bool]$token.SeBackupEnabled
        se_restore_available = [bool]$token.SeRestoreAvailable
        se_restore_enabled = [bool]$token.SeRestoreEnabled
        se_security_available = [bool]$token.SeSecurityAvailable
        se_security_enabled = [bool]$token.SeSecurityEnabled
        robocopy_contract = [bool]$robocopy_ok
        directory_symlink = [bool]$symlink_ok
        codes = @($codes.ToArray())
    }
}
catch {
    $report = [ordered]@{
        schema_version = 1
        status = 'blocked'
        evidence_class = 'local_prerequisites_only'
        process_token_all_access = $false
        thread_not_impersonating = $false
        primary_process_token = $false
        token_has_restrictions_zero = $false
        is_token_restricted_false = $false
        logon_sid_count = 0
        enabled_non_deny_logon_sid_count = 0
        privileges_query_succeeded = $false
        se_backup_available = $false
        se_backup_enabled = $false
        se_restore_available = $false
        se_restore_enabled = $false
        se_security_available = $false
        se_security_enabled = $false
        robocopy_contract = $false
        directory_symlink = $false
        codes = @('probe_internal_failure')
    }
}

[Console]::Out.WriteLine(($report | ConvertTo-Json -Compress -Depth 3))
