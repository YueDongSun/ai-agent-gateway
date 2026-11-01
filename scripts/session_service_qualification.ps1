<#
.SYNOPSIS
Plan, apply, or roll back the isolated Windows session-service qualification.
.DESCRIPTION
The default path emits JSON and performs no mutations or native compilation.
SelfTest runs in-memory receipt/recovery fixtures without loading native code.
Apply requires elevation and the explicit RunId/SourceKey from a reviewed plan.
The protected journal is retained as a physical-generation non-reuse fence.
This script never reads gateway credentials or changes existing services.
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [switch]$Rollback,
    [switch]$SelfTest,
    [string]$SourceBinary,
    [string]$NativeExecutable,
    [string]$RunId,
    [string]$SourceKey,
    [UInt64]$Generation = 1,
    [string]$CanonicalId,
    [string]$ManifestPath,
    [ValidateRange(5,120)][int]$WaitSeconds = 50
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:journal = $null
$script:journal_path = $null
$script:lock_stream = $null
$script:lock_owned = $false
$script:stage = 'plan'
$script:marker = 'aag_session_service_qualification_v1'
$script:policy_rights = @('SeBackupPrivilege','SeRestorePrivilege','SeSecurityPrivilege')
$script:policy_privileges = @('SeChangeNotifyPrivilege') + $script:policy_rights
$script:minimal_privileges = @('SeChangeNotifyPrivilege')
$script:max_tree_entries = 4096

function fail([string]$code) { throw [InvalidOperationException]::new($code) }

function plain_path([string]$value) {
    if ([string]::IsNullOrWhiteSpace($value) -or $value -notmatch '^[A-Za-z]:[\\/]' -or
        $value -match '[\x00-\x1f"<>|?*]' -or $value.Substring(2).Contains(':') -or
        ($value -split '[\\/]') -contains '..' -or ($value -split '[\\/]') -contains '.') {
        fail 'session_qualification_path_invalid'
    }
    [IO.Path]::GetFullPath($value).TrimEnd([char[]]'\/')
}

function within_path([string]$path,[string]$root) {
    $path.Equals($root,[StringComparison]::OrdinalIgnoreCase) -or
        $path.StartsWith($root + '\',[StringComparison]::OrdinalIgnoreCase)
}

function no_reparse([string]$path) {
    $cursor = $path
    while (-not [string]::IsNullOrEmpty($cursor)) {
        if ([IO.File]::Exists($cursor) -or [IO.Directory]::Exists($cursor)) {
            if (([IO.File]::GetAttributes($cursor) -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                fail 'session_qualification_reparse_forbidden'
            }
        }
        $parent = [IO.Path]::GetDirectoryName($cursor)
        if ($parent -eq $cursor) { break }
        $cursor = $parent
    }
}

function quote_argument([string]$value) {
    if ($value.Contains([char]0) -or $value -match '[\r\n]') { fail 'session_qualification_argument_invalid' }
    '"' + ([regex]::Replace($value,'(\\*)"','$1$1\"') -replace '(\\+)$','$1$1') + '"'
}

function image_path([string]$binary,[string[]]$arguments) {
    (@(quote_argument $binary) + @($arguments | ForEach-Object { quote_argument $_ })) -join ' '
}

function directory_sddl([string]$owner,[string[]]$readers = @()) {
    $value = "O:${owner}D:P(A;OICI;FA;;;${owner})"
    foreach ($sid in @('S-1-5-18','S-1-5-32-544') | Select-Object -Unique) {
        if ($sid -ne $owner) { $value += "(A;OICI;FA;;;${sid})" }
    }
    foreach ($sid in $readers | Select-Object -Unique) {
        if ($sid -notin @($owner,'S-1-5-18','S-1-5-32-544')) { $value += "(A;OICI;0x1200a9;;;${sid})" }
    }
    $value
}

function file_sddl([string[]]$readers = @()) {
    $value = 'O:BAD:P(A;;FA;;;BA)(A;;FA;;;SY)'
    foreach ($sid in $readers | Select-Object -Unique) { $value += "(A;;0x1200a9;;;${sid})" }
    $value
}

function same_sddl([string]$actual,[string]$expected) {
    $actual_sd = [Security.AccessControl.RawSecurityDescriptor]::new($actual)
    $expected_sd = [Security.AccessControl.RawSecurityDescriptor]::new($expected)
    if ($actual_sd.Owner.Value -ne $expected_sd.Owner.Value -or
        ($actual_sd.ControlFlags -band [Security.AccessControl.ControlFlags]::DiscretionaryAclProtected) -eq 0) { return $false }
    $actual_aces = @($actual_sd.DiscretionaryAcl | ForEach-Object { $_.GetType().Name + ':' + $_.AceType + ':' + $_.AceFlags + ':' + $_.AccessMask + ':' + $_.SecurityIdentifier.Value } | Sort-Object)
    $expected_aces = @($expected_sd.DiscretionaryAcl | ForEach-Object { $_.GetType().Name + ':' + $_.AceType + ':' + $_.AceFlags + ':' + $_.AccessMask + ':' + $_.SecurityIdentifier.Value } | Sort-Object)
    ($actual_aces -join '|') -ceq ($expected_aces -join '|')
}

function build_plan {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or -not [Environment]::Is64BitProcess) {
        fail 'session_qualification_64bit_windows_required'
    }
    $program_files = plain_path ([Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles))
    $program_data = plain_path ([Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData))
    if (-not $RunId) { $RunId = [Guid]::NewGuid().ToString('N') }
    if (-not $SourceKey) { $SourceKey = [Guid]::NewGuid().ToString('N') }
    if ($RunId -cnotmatch '^[0-9a-f]{32}$' -or $SourceKey -cnotmatch '^[0-9a-f]{32}$' -or $Generation -eq 0) {
        fail 'session_qualification_identity_invalid'
    }
    if (-not $CanonicalId) { $CanonicalId = 'qualification_' + $SourceKey }
    if ($CanonicalId -cnotmatch '^[A-Za-z0-9_-]{1,128}$') { fail 'session_qualification_canonical_id_invalid' }
    if (-not $SourceBinary) { $SourceBinary = Join-Path (Split-Path -Parent $PSScriptRoot) 'src-tauri\target\debug\aag_session_authority.exe' }
    $source = plain_path $SourceBinary
    if ([IO.Path]::GetFileName($source) -cne 'aag_session_authority.exe') { fail 'session_qualification_binary_name_invalid' }
    no_reparse $source
    $native = $null
    if ($NativeExecutable) {
        $native = plain_path $NativeExecutable
        if (-not (within_path $native $program_files) -or [IO.Path]::GetFileName($native) -ine 'codex.exe') {
            fail 'session_qualification_native_path_invalid'
        }
        no_reparse $native
    }
    $artifact_root = Join-Path $program_files 'AI Agent Gateway\SessionAuthority'
    $data_root = Join-Path $program_data 'AI Agent Gateway\SessionAuthority'
    $qualification_root = Join-Path $data_root 'qualification'
    $destination = Join-Path $artifact_root 'aag_session_authority.exe'
    $names = @('aag_session_authority','aag_session_reader',('aag_session_runtime_' + $SourceKey + '_g' + $Generation))
    $roles = @('policy','reader','runtime')
    $services = for ($index=0; $index -lt 3; $index++) {
        [ordered]@{
            name=$names[$index]; role=$roles[$index]; account=('NT SERVICE\' + $names[$index]);
            directory=(Join-Path $qualification_root $names[$index]);
            display_tag=($script:marker + ' ' + $RunId + ' ' + $roles[$index]);
            privileges=$(if ($index -eq 0) { $script:policy_privileges } else { $script:minimal_privileges });
            bootstrap_image=(image_path $destination @('qualification-unconfigured','--run-id',$RunId));
            final_image=$null; expected_sid=$null; create_state='planned'; configured=$false; started=$false; removed=$false
            process_id=0
        }
    }
    [ordered]@{
        schema_version=1; owner=$script:marker; run_id=$RunId; source_key=$SourceKey;
        generation=$Generation; generation_text=$Generation.ToString([Globalization.CultureInfo]::InvariantCulture);
        canonical_id=$CanonicalId; program_files=$program_files; program_data=$program_data;
        source_binary=$source; source_binary_exists=[IO.File]::Exists($source); native_executable=$native;
        artifact_root=$artifact_root; data_root=$data_root; qualification_root=$qualification_root; binary=$destination;
        manifest_path=(Join-Path $program_data ('aag_session_qualification_' + $RunId + '.json'));
        lock_path=(Join-Path $program_data 'aag_session_qualification_operation.lock');
        phase='planned'; copy_integrity=$false; binary_sha256=$null; directories=@(); services=@($services);
        binary_acl=[ordered]@{initial_intended=$false;initial_verified=$false;readers_intended=$false;readers_verified=$false;reader_sids=@()};
        policy_rights=[ordered]@{ sid=$null; before=@(); added=@(); intent=$false; restored=$false };
        events=@(); receipts=@(); rollback_complete=$false
    }
}

function public_plan($plan,[string]$mode) {
    [ordered]@{
        mode=$mode; mutations=$false; run_id=$plan.run_id; source_key=$plan.source_key;
        generation=$plan.generation; generation_text=$plan.generation_text; canonical_id=$plan.canonical_id;
        source_binary=$plan.source_binary; source_binary_exists=$plan.source_binary_exists;
        protected_binary=$plan.binary; native_executable=$plan.native_executable;
        manifest_path=$plan.manifest_path; qualification_root=$plan.qualification_root;
        journal_location='protected_flat_ProgramData_file_written_before_nested_resources';
        services=@($plan.services | ForEach-Object { [ordered]@{name=$_.name;role=$_.role;account=$_.account;directory=$_.directory;privileges=@($_.privileges);start='manual';type='own_process';sid_type='unrestricted'} });
        policy_lsa_rights=$script:policy_rights;
        preconditions=@('explicit_apply_and_reviewed_run_id_source_key','elevated_operator','fresh_service_names_and_artifact_roots','protected_source_binary_copy_verified','no_reparse_or_unknown_existing_resources');
        untouched=@('existing_Tailscale_AAG_Codex_services','gateway_credentials','live_homes','firewall_network_provider_settings');
        rollback='exact_journal_owned_resources_only_never_reuse_runtime_generation'
    }
}

function load_native_adapter {
    if ('AagSessionQualificationNative' -as [type]) { return }
    $native_source = @'
using System;
using System.IO;
using System.Text;
using System.Linq;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using Microsoft.Win32.SafeHandles;

public static class AagSessionQualificationNative {
    const uint query_config=1, change_config=2, query_status=4, service_start=16, service_stop=32, delete_access=0x10000;
    const uint read_control=0x20000, write_dac=0x40000, write_owner=0x80000;
    static readonly string[] policy_rights = {"SeBackupPrivilege","SeRestorePrivilege","SeSecurityPrivilege"};
    [StructLayout(LayoutKind.Sequential)] struct luid { public uint low; public int high; }
    [StructLayout(LayoutKind.Sequential)] struct token_privilege { public uint count; public luid id; public uint attributes; }
    [StructLayout(LayoutKind.Sequential)] struct security_attributes { public int length; public IntPtr descriptor; public int inherit; }
    [StructLayout(LayoutKind.Sequential)] struct query_config_record { public uint type,start,error; public IntPtr binary,load_group; public uint tag; public IntPtr dependencies,account,display; }
    [StructLayout(LayoutKind.Sequential)] struct status_record { public uint type,state,controls,win32_exit,specific_exit,checkpoint,hint,pid,flags; }
    [StructLayout(LayoutKind.Sequential)] struct service_pointer { public IntPtr value; }
    [StructLayout(LayoutKind.Sequential)] struct service_sid { public uint kind; }
    [StructLayout(LayoutKind.Sequential)] struct lsa_string { public ushort length,maximum; public IntPtr buffer; }
    [StructLayout(LayoutKind.Sequential)] struct lsa_attributes { public uint length; public IntPtr root,name; public uint attributes; public IntPtr descriptor,qos; }
    [StructLayout(LayoutKind.Sequential)] struct file_information { public uint attributes; public System.Runtime.InteropServices.ComTypes.FILETIME creation,access,write; public uint volume,size_high,size_low,links,index_high,index_low; }
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr OpenSCManagerW(string machine,string database,uint access);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr OpenServiceW(IntPtr manager,string name,uint access);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr CreateServiceW(IntPtr manager,string name,string display,uint access,uint type,uint start,uint error,string binary,string group,IntPtr tag,string dependencies,string account,string password);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool QueryServiceConfigW(IntPtr service,IntPtr buffer,uint length,out uint needed);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ChangeServiceConfigW(IntPtr service,uint type,uint start,uint error,string binary,string group,IntPtr tag,string dependencies,string account,string password,string display);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool QueryServiceConfig2W(IntPtr service,uint level,IntPtr buffer,uint length,out uint needed);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ChangeServiceConfig2W(IntPtr service,uint level,IntPtr value);
    [DllImport("advapi32",SetLastError=true)] static extern bool QueryServiceStatusEx(IntPtr service,uint level,out status_record status,uint size,out uint needed);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool StartServiceW(IntPtr service,uint count,IntPtr arguments);
    [DllImport("advapi32",SetLastError=true)] static extern bool ControlService(IntPtr service,uint control,out status_record status);
    [DllImport("advapi32",SetLastError=true)] static extern bool DeleteService(IntPtr service);
    [DllImport("advapi32",SetLastError=true)] static extern bool QueryServiceObjectSecurity(IntPtr service,uint information,IntPtr descriptor,uint length,out uint needed);
    [DllImport("advapi32",SetLastError=true)] static extern bool SetServiceObjectSecurity(IntPtr service,uint information,IntPtr descriptor);
    [DllImport("advapi32")] static extern bool CloseServiceHandle(IntPtr handle);
    [DllImport("kernel32")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32",SetLastError=true)] static extern IntPtr OpenProcess(uint access,bool inherit,uint pid);
    [DllImport("kernel32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool QueryFullProcessImageNameW(IntPtr process,uint flags,StringBuilder path,ref uint length);
    [DllImport("kernel32",SetLastError=true)] static extern uint WaitForSingleObject(IntPtr handle,uint milliseconds);
    [DllImport("kernel32")] static extern void SetLastError(uint error);
    [DllImport("advapi32",SetLastError=true)] static extern bool OpenProcessToken(IntPtr process,uint access,out IntPtr token);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool LookupPrivilegeValueW(string system,string name,out luid id);
    [DllImport("advapi32",SetLastError=true)] static extern bool AdjustTokenPrivileges(IntPtr token,bool disable,ref token_privilege current,uint size,out token_privilege previous,out uint needed);
    [DllImport("advapi32",SetLastError=true)] static extern bool AdjustTokenPrivileges(IntPtr token,bool disable,ref token_privilege current,uint size,IntPtr previous,IntPtr needed);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string text,uint revision,out IntPtr descriptor,out uint size);
    [DllImport("advapi32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool ConvertSecurityDescriptorToStringSecurityDescriptorW(IntPtr descriptor,uint revision,uint information,out IntPtr text,out uint size);
    [DllImport("advapi32",SetLastError=true)] static extern bool GetSecurityDescriptorOwner(IntPtr descriptor,out IntPtr owner,out bool defaulted);
    [DllImport("advapi32",SetLastError=true)] static extern bool GetSecurityDescriptorDacl(IntPtr descriptor,out bool present,out IntPtr acl,out bool defaulted);
    [DllImport("advapi32")] static extern uint GetSecurityInfo(IntPtr handle,uint type,uint information,out IntPtr owner,out IntPtr group,out IntPtr dacl,out IntPtr sacl,out IntPtr descriptor);
    [DllImport("advapi32")] static extern uint SetSecurityInfo(IntPtr handle,uint type,uint information,IntPtr owner,IntPtr group,IntPtr dacl,IntPtr sacl);
    [DllImport("kernel32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool CreateDirectoryW(string path,ref security_attributes attributes);
    [DllImport("kernel32",CharSet=CharSet.Unicode,SetLastError=true)] static extern SafeFileHandle CreateFileW(string path,uint access,uint share,IntPtr attributes,uint creation,uint flags,IntPtr template);
    [DllImport("kernel32",CharSet=CharSet.Unicode,SetLastError=true)] static extern SafeFileHandle CreateFileW(string path,uint access,uint share,ref security_attributes attributes,uint creation,uint flags,IntPtr template);
    [DllImport("kernel32",SetLastError=true)] static extern bool GetFileInformationByHandle(SafeFileHandle handle,out file_information information);
    [DllImport("kernel32",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool MoveFileExW(string from,string to,uint flags);
    [DllImport("kernel32")] static extern IntPtr LocalFree(IntPtr memory);
    [DllImport("advapi32")] static extern uint LsaOpenPolicy(IntPtr system,ref lsa_attributes attributes,uint access,out IntPtr policy);
    [DllImport("advapi32")] static extern uint LsaEnumerateAccountRights(IntPtr policy,IntPtr sid,out IntPtr rights,out uint count);
    [DllImport("advapi32")] static extern uint LsaAddAccountRights(IntPtr policy,IntPtr sid,lsa_string[] rights,uint count);
    [DllImport("advapi32")] static extern uint LsaRemoveAccountRights(IntPtr policy,IntPtr sid,byte all,lsa_string[] rights,uint count);
    [DllImport("advapi32")] static extern uint LsaNtStatusToWinError(uint status);
    [DllImport("advapi32")] static extern uint LsaClose(IntPtr policy);
    [DllImport("advapi32")] static extern uint LsaFreeMemory(IntPtr memory);

    static void error(string stage) { throw new InvalidOperationException("session_qualification_"+stage+"_win32_"+Marshal.GetLastWin32Error()); }
    static void status_error(string stage,uint value) { throw new InvalidOperationException("session_qualification_"+stage+"_win32_"+value); }
    static void name_ok(string name) {
        if(name=="aag_session_authority" || name=="aag_session_reader") return;
        if(!System.Text.RegularExpressions.Regex.IsMatch(name,@"^aag_session_runtime_[0-9a-f]{32}_g[1-9][0-9]{0,19}$")) throw new InvalidOperationException("session_qualification_service_name_invalid");
        ulong generation; if(!UInt64.TryParse(name.Substring(name.LastIndexOf("_g",StringComparison.Ordinal)+2),out generation) || generation==0) throw new InvalidOperationException("session_qualification_generation_invalid");
    }
    static string full(string path) {
        string value=Path.GetFullPath(path).TrimEnd('\\');
        if(value.Length<3 || value[1]!=':' || value.StartsWith(@"\\") || value.Substring(2).Contains(":") || value.IndexOfAny(new char[]{'\r','\n','\0'})>=0) throw new InvalidOperationException("session_qualification_path_invalid");
        return value;
    }
    static bool under(string path,string root) { return path.Equals(root,StringComparison.OrdinalIgnoreCase) || path.StartsWith(root+@"\",StringComparison.OrdinalIgnoreCase); }
    static void scope(string path) {
        string value=full(path), pf=Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), pd=Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData);
        bool journal=Path.GetDirectoryName(value).Equals(pd,StringComparison.OrdinalIgnoreCase) && System.Text.RegularExpressions.Regex.IsMatch(Path.GetFileName(value),@"^aag_session_qualification_([0-9a-f]{32}\.json(\.new_[0-9a-f]{32})?|operation\.lock)$");
        if(!journal && !under(value,Path.Combine(pf,@"AI Agent Gateway\SessionAuthority")) && !under(value,Path.Combine(pd,@"AI Agent Gateway\SessionAuthority")) && !value.Equals(Path.Combine(pf,"AI Agent Gateway"),StringComparison.OrdinalIgnoreCase) && !value.Equals(Path.Combine(pd,"AI Agent Gateway"),StringComparison.OrdinalIgnoreCase)) throw new InvalidOperationException("session_qualification_scope_invalid");
        no_links(value);
    }
    static void no_links(string path) {
        string cursor=full(path);
        while(!String.IsNullOrEmpty(cursor)) {
            if((File.Exists(cursor)||Directory.Exists(cursor)) && (File.GetAttributes(cursor)&FileAttributes.ReparsePoint)!=0) throw new InvalidOperationException("session_qualification_reparse_forbidden");
            cursor=Path.GetDirectoryName(cursor);
        }
    }
    sealed class privilege_scope : IDisposable {
        IntPtr token; token_privilege previous; bool changed;
        public privilege_scope(string name) {
            if(name!="SeBackupPrivilege" && name!="SeRestorePrivilege") throw new InvalidOperationException("session_qualification_operator_privilege_forbidden");
            if(!OpenProcessToken(GetCurrentProcess(),0x28,out token)) error("operator_token");
            try {
                token_privilege current=new token_privilege(); current.count=1; current.attributes=2;
                if(!LookupPrivilegeValueW(null,name,out current.id)) error("operator_privilege_lookup");
                uint needed; SetLastError(0);
                if(!AdjustTokenPrivileges(token,false,ref current,(uint)Marshal.SizeOf(typeof(token_privilege)),out previous,out needed)) error("operator_privilege_enable");
                uint code=(uint)Marshal.GetLastWin32Error(); changed=true;
                if(code!=0) status_error("operator_privilege_enable",code);
            } catch { Dispose(); throw; }
        }
        public void Dispose() {
            if(token!=IntPtr.Zero) {
                uint restore_error=0;
                if(changed) { SetLastError(0);bool restored=AdjustTokenPrivileges(token,false,ref previous,0,IntPtr.Zero,IntPtr.Zero);restore_error=(uint)Marshal.GetLastWin32Error();if(!restored && restore_error==0)restore_error=31;changed=false; }
                CloseHandle(token); token=IntPtr.Zero;
                if(restore_error!=0)status_error("operator_privilege_restore",restore_error);
            }
        }
    }
    public sealed class service_snapshot {
        public string name,binary,account,display,security; public uint type,start,state,pid,sid_type,win32_exit,specific_exit; public string[] privileges;
    }
    static IntPtr manager(uint access) { IntPtr handle=OpenSCManagerW(null,null,access); if(handle==IntPtr.Zero) error("scm_open"); return handle; }
    static IntPtr service(IntPtr scm,string name,uint access) { name_ok(name); IntPtr handle=OpenServiceW(scm,name,access); if(handle==IntPtr.Zero) error("service_open"); return handle; }
    static IntPtr config_buffer(IntPtr svc,uint level) {
        uint needed; QueryServiceConfig2W(svc,level,IntPtr.Zero,0,out needed);
        if(needed==0 || needed>65536) throw new InvalidOperationException("session_qualification_service_config_invalid");
        IntPtr buffer=Marshal.AllocHGlobal((int)needed);
        if(!QueryServiceConfig2W(svc,level,buffer,needed,out needed)) { Marshal.FreeHGlobal(buffer); error("service_config_read"); }
        return buffer;
    }
    public static service_snapshot query_service(string name) {
        name_ok(name); IntPtr scm=manager(1),svc=IntPtr.Zero,buffer=IntPtr.Zero;
        try {
            svc=OpenServiceW(scm,name,query_config|query_status|read_control);
            if(svc==IntPtr.Zero) { int code=Marshal.GetLastWin32Error(); if(code==1060) return null; status_error("service_query",(uint)code); }
            uint needed; QueryServiceConfigW(svc,IntPtr.Zero,0,out needed);
            if(needed==0||needed>65536) throw new InvalidOperationException("session_qualification_service_config_invalid");
            buffer=Marshal.AllocHGlobal((int)needed);
            if(!QueryServiceConfigW(svc,buffer,needed,out needed)) error("service_config_read");
            query_config_record cfg=(query_config_record)Marshal.PtrToStructure(buffer,typeof(query_config_record));
            status_record state; if(!QueryServiceStatusEx(svc,0,out state,(uint)Marshal.SizeOf(typeof(status_record)),out needed)) error("service_status_read");
            service_snapshot result=new service_snapshot(); result.name=name;result.binary=Marshal.PtrToStringUni(cfg.binary);result.account=Marshal.PtrToStringUni(cfg.account);result.display=Marshal.PtrToStringUni(cfg.display);result.type=cfg.type;result.start=cfg.start;result.state=state.state;result.pid=state.pid;result.win32_exit=state.win32_exit;result.specific_exit=state.specific_exit;
            IntPtr extra=config_buffer(svc,5); try { result.sid_type=(uint)Marshal.ReadInt32(extra); } finally { Marshal.FreeHGlobal(extra); }
            extra=config_buffer(svc,6); try {
                IntPtr cursor=Marshal.ReadIntPtr(extra); List<string> rights=new List<string>(); int consumed=0;
                while(cursor!=IntPtr.Zero && Marshal.ReadInt16(cursor)!=0) { string value=Marshal.PtrToStringUni(cursor); consumed+=value.Length+1; if(consumed>8192) throw new InvalidOperationException("session_qualification_privileges_invalid"); rights.Add(value); cursor=IntPtr.Add(cursor,(value.Length+1)*2); }
                result.privileges=rights.ToArray();
            } finally { Marshal.FreeHGlobal(extra); }
            QueryServiceObjectSecurity(svc,5,IntPtr.Zero,0,out needed);if(needed==0||needed>65536)throw new InvalidOperationException("session_qualification_service_security_bounds");extra=Marshal.AllocHGlobal((int)needed);IntPtr security_text=IntPtr.Zero;
            try{if(!QueryServiceObjectSecurity(svc,5,extra,needed,out needed))error("service_security_read");uint chars;if(!ConvertSecurityDescriptorToStringSecurityDescriptorW(extra,1,5,out security_text,out chars))error("service_security_text");result.security=Marshal.PtrToStringUni(security_text);}
            finally{if(security_text!=IntPtr.Zero)LocalFree(security_text);Marshal.FreeHGlobal(extra);}
            return result;
        } finally { if(buffer!=IntPtr.Zero) Marshal.FreeHGlobal(buffer); if(svc!=IntPtr.Zero) CloseServiceHandle(svc); CloseServiceHandle(scm); }
    }
    public static void create_service(string name,string display,string image) {
        name_ok(name); IntPtr scm=manager(3),svc=IntPtr.Zero;
        try { svc=CreateServiceW(scm,name,display,0xF01FF,16,3,1,image,null,IntPtr.Zero,null,@"NT SERVICE\"+name,null); if(svc==IntPtr.Zero) error("service_create");IntPtr sd=descriptor("O:BAD:P(A;;GA;;;BA)(A;;GA;;;SY)");try{if(!SetServiceObjectSecurity(svc,0x80000005,sd))error("service_security_set");}finally{LocalFree(sd);} }
        finally { if(svc!=IntPtr.Zero) CloseServiceHandle(svc); CloseServiceHandle(scm); }
    }
    public static void configure_service(string name,string image,string[] privileges) {
        name_ok(name); string[] expected=name=="aag_session_authority" ? new string[]{"SeChangeNotifyPrivilege","SeBackupPrivilege","SeRestorePrivilege","SeSecurityPrivilege"} : new string[]{"SeChangeNotifyPrivilege"};
        if(!expected.OrderBy(x=>x).SequenceEqual(privileges.OrderBy(x=>x))) throw new InvalidOperationException("session_qualification_privileges_invalid");
        IntPtr scm=manager(1),svc=IntPtr.Zero,text=IntPtr.Zero,record=IntPtr.Zero;
        try {
            svc=service(scm,name,change_config);
            if(!ChangeServiceConfigW(svc,0xFFFFFFFF,3,0xFFFFFFFF,image,null,IntPtr.Zero,null,null,null,null)) error("service_image_configure");
            record=Marshal.AllocHGlobal(Marshal.SizeOf(typeof(service_sid))); Marshal.StructureToPtr(new service_sid{kind=1},record,false);
            if(!ChangeServiceConfig2W(svc,5,record)) error("service_sid_configure");
            Marshal.FreeHGlobal(record);record=IntPtr.Zero;
            text=Marshal.StringToHGlobalUni(String.Join("\0",privileges)+"\0\0"); record=Marshal.AllocHGlobal(IntPtr.Size);Marshal.WriteIntPtr(record,text);
            if(!ChangeServiceConfig2W(svc,6,record)) error("service_privileges_configure");
        } finally { if(record!=IntPtr.Zero) Marshal.FreeHGlobal(record);if(text!=IntPtr.Zero) Marshal.FreeHGlobal(text);if(svc!=IntPtr.Zero) CloseServiceHandle(svc);CloseServiceHandle(scm); }
    }
    public static void protect_service(string name,string policy_sid) {
        name_ok(name);if(policy_sid!=service_sid_value("aag_session_authority"))throw new InvalidOperationException("session_qualification_policy_sid_mismatch");
        IntPtr scm=manager(1),svc=IntPtr.Zero,sd=IntPtr.Zero;
        try{svc=service(scm,name,write_dac|write_owner);sd=descriptor("O:BAD:P(A;;GA;;;BA)(A;;GA;;;SY)(A;;GA;;;"+policy_sid+")");if(!SetServiceObjectSecurity(svc,0x80000005,sd))error("service_security_set");}
        finally{if(sd!=IntPtr.Zero)LocalFree(sd);if(svc!=IntPtr.Zero)CloseServiceHandle(svc);CloseServiceHandle(scm);}
    }
    public static void control_service(string name,string action) {
        name_ok(name); IntPtr scm=manager(1),svc=IntPtr.Zero;
        try {
            svc=service(scm,name,action=="start"?service_start:action=="stop"?service_stop:action=="delete"?delete_access:0);
            if(action=="start") { if(!StartServiceW(svc,0,IntPtr.Zero)) error("service_start"); }
            else if(action=="stop") { status_record status;if(!ControlService(svc,1,out status)) error("service_stop"); }
            else if(action=="delete") { if(!DeleteService(svc)) error("service_delete"); }
            else throw new InvalidOperationException("session_qualification_control_invalid");
        } finally { if(svc!=IntPtr.Zero) CloseServiceHandle(svc);CloseServiceHandle(scm); }
    }
    public static bool service_present(string name) {
        name_ok(name);IntPtr scm=manager(1),svc=IntPtr.Zero;
        try {svc=OpenServiceW(scm,name,query_status);if(svc!=IntPtr.Zero)return true;int code=Marshal.GetLastWin32Error();if(code==1060)return false;if(code==1072)return true;status_error("service_query",(uint)code);return true;}
        finally {if(svc!=IntPtr.Zero)CloseServiceHandle(svc);CloseServiceHandle(scm);}
    }
    public sealed class process_lease : IDisposable {
        IntPtr handle;
        internal process_lease(uint pid,string expected) {
            scope(expected);handle=OpenProcess(0x00101000,false,pid);if(handle==IntPtr.Zero)error("process_query_open");
            try{StringBuilder path=new StringBuilder(32768);uint length=(uint)path.Capacity;if(!QueryFullProcessImageNameW(handle,0,path,ref length))error("process_image_read");if(!full(path.ToString()).Equals(full(expected),StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("session_qualification_process_image_mismatch");}
            catch{Dispose();throw;}
        }
        public bool wait_exit(uint milliseconds){uint result=WaitForSingleObject(handle,milliseconds);if(result==0)return true;if(result==258)return false;error("process_exit_wait");return false;}
        public void Dispose(){if(handle!=IntPtr.Zero){CloseHandle(handle);handle=IntPtr.Zero;}}
    }
    public static process_lease lease_process(uint pid,string expected){return new process_lease(pid,expected);}
    public static string service_sid_value(string name) { name_ok(name); return ((SecurityIdentifier)new NTAccount(@"NT SERVICE\"+name).Translate(typeof(SecurityIdentifier))).Value; }
    static byte[] sid_bytes(string sid) { SecurityIdentifier value=new SecurityIdentifier(sid);byte[] bytes=new byte[value.BinaryLength];value.GetBinaryForm(bytes,0);return bytes; }
    public static string[] policy_assigned_rights(string sid) {
        if(sid!=service_sid_value("aag_session_authority")) throw new InvalidOperationException("session_qualification_policy_sid_mismatch");
        lsa_attributes attributes=new lsa_attributes();attributes.length=(uint)Marshal.SizeOf(typeof(lsa_attributes));IntPtr policy;uint code=LsaOpenPolicy(IntPtr.Zero,ref attributes,0x800,out policy);if(code!=0) status_error("lsa_open",LsaNtStatusToWinError(code));
        GCHandle pinned=GCHandle.Alloc(sid_bytes(sid),GCHandleType.Pinned);IntPtr rights=IntPtr.Zero;
        try { uint count;code=LsaEnumerateAccountRights(policy,pinned.AddrOfPinnedObject(),out rights,out count);if(code==0xC0000034) return new string[0];if(code!=0) status_error("lsa_read",LsaNtStatusToWinError(code));if(count>256) throw new InvalidOperationException("session_qualification_lsa_bounds");List<string> values=new List<string>();for(uint i=0;i<count;i++){lsa_string entry=(lsa_string)Marshal.PtrToStructure(IntPtr.Add(rights,(int)i*Marshal.SizeOf(typeof(lsa_string))),typeof(lsa_string));string value=Marshal.PtrToStringUni(entry.buffer,entry.length/2);if(policy_rights.Contains(value))values.Add(value);else if(value!="SeServiceLogonRight"&&value!="SeChangeNotifyPrivilege")throw new InvalidOperationException("session_qualification_preexisting_policy_rights_forbidden");}return values.ToArray(); }
        finally { if(rights!=IntPtr.Zero)LsaFreeMemory(rights);pinned.Free();LsaClose(policy); }
    }
    public static void change_policy_rights(string sid,string[] rights,bool add) {
        if(sid!=service_sid_value("aag_session_authority") || rights.Any(x=>!policy_rights.Contains(x))) throw new InvalidOperationException("session_qualification_policy_rights_forbidden");
        if(rights.Length==0)return;
        lsa_attributes attributes=new lsa_attributes();attributes.length=(uint)Marshal.SizeOf(typeof(lsa_attributes));IntPtr policy;uint code=LsaOpenPolicy(IntPtr.Zero,ref attributes,0x810,out policy);if(code!=0)status_error("lsa_open",LsaNtStatusToWinError(code));
        GCHandle pinned=GCHandle.Alloc(sid_bytes(sid),GCHandleType.Pinned);lsa_string[] entries=new lsa_string[rights.Length];
        try { for(int i=0;i<rights.Length;i++){entries[i].buffer=Marshal.StringToHGlobalUni(rights[i]);entries[i].length=(ushort)(rights[i].Length*2);entries[i].maximum=(ushort)(entries[i].length+2);}code=add?LsaAddAccountRights(policy,pinned.AddrOfPinnedObject(),entries,(uint)entries.Length):LsaRemoveAccountRights(policy,pinned.AddrOfPinnedObject(),0,entries,(uint)entries.Length);if(code!=0)status_error("lsa_change",LsaNtStatusToWinError(code)); }
        finally { foreach(lsa_string value in entries)if(value.buffer!=IntPtr.Zero)Marshal.FreeHGlobal(value.buffer);pinned.Free();LsaClose(policy); }
    }
    static IntPtr descriptor(string sddl) { IntPtr value;uint size;if(!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl,1,out value,out size))error("descriptor");return value; }
    public static void create_directory(string path,string sddl) { scope(path);IntPtr sd=descriptor(sddl);try{security_attributes attributes=new security_attributes{length=Marshal.SizeOf(typeof(security_attributes)),descriptor=sd,inherit=0};using(privilege_scope privilege=new privilege_scope("SeRestorePrivilege")){if(!CreateDirectoryW(path,ref attributes))error("directory_create");}}finally{LocalFree(sd);} }
    public static FileStream create_file_stream(string path,string sddl) {scope(path);IntPtr sd=descriptor(sddl);try{security_attributes attributes=new security_attributes{length=Marshal.SizeOf(typeof(security_attributes)),descriptor=sd,inherit=0};SafeFileHandle handle=CreateFileW(path,0x40000000,0,ref attributes,1,0x80000080,IntPtr.Zero);if(handle.IsInvalid){handle.Dispose();error("file_create");}try{return new FileStream(handle,FileAccess.Write);}catch{handle.Dispose();throw;}}finally{LocalFree(sd);} }
    public static void write_new_file(string path,byte[] bytes,string sddl) {using(FileStream stream=create_file_stream(path,sddl)){stream.Write(bytes,0,bytes.Length);stream.Flush(true);} }
    public static void replace_journal(string temporary,string target) { scope(temporary);scope(target);if(!MoveFileExW(temporary,target,9))error("journal_replace"); }
    static SafeFileHandle open_backup(string path,uint access) { scope(path);SafeFileHandle handle=CreateFileW(path,access|0x80,7,IntPtr.Zero,3,0x02200000,IntPtr.Zero);if(handle.IsInvalid){handle.Dispose();error("backup_open");}file_information info;if(!GetFileInformationByHandle(handle,out info)){handle.Dispose();error("file_metadata");}if((info.attributes&0x400)!=0){handle.Dispose();throw new InvalidOperationException("session_qualification_reparse_forbidden");}return handle; }
    public sealed class file_metadata { public string owner;public uint links;public bool directory; }
    public static file_metadata metadata(string path) { using(privilege_scope privilege=new privilege_scope("SeBackupPrivilege"))using(SafeFileHandle handle=open_backup(path,read_control)){file_information info;if(!GetFileInformationByHandle(handle,out info))error("file_metadata");IntPtr owner,group,acl,sacl,sd;uint code=GetSecurityInfo(handle.DangerousGetHandle(),1,1,out owner,out group,out acl,out sacl,out sd);if(code!=0)status_error("file_owner",code);try{return new file_metadata{owner=new SecurityIdentifier(owner).Value,links=info.links,directory=(info.attributes&0x10)!=0};}finally{LocalFree(sd);}} }
    public static string read_sddl(string path) { using(privilege_scope privilege=new privilege_scope("SeBackupPrivilege"))using(SafeFileHandle handle=open_backup(path,read_control)){IntPtr owner,group,acl,sacl,sd;uint code=GetSecurityInfo(handle.DangerousGetHandle(),1,5,out owner,out group,out acl,out sacl,out sd);if(code!=0)status_error("file_security_read",code);IntPtr text=IntPtr.Zero;try{uint length;if(!ConvertSecurityDescriptorToStringSecurityDescriptorW(sd,1,5,out text,out length))error("descriptor_text");return Marshal.PtrToStringUni(text);}finally{if(text!=IntPtr.Zero)LocalFree(text);LocalFree(sd);}} }
    public static void apply_sddl(string path,string sddl) {
        scope(path);IntPtr sd=descriptor(sddl);
        try {
            IntPtr owner=IntPtr.Zero,acl=IntPtr.Zero;bool defaulted=false,present=false;
            if(!GetSecurityDescriptorOwner(sd,out owner,out defaulted))error("descriptor_owner");
            if(!GetSecurityDescriptorDacl(sd,out present,out acl,out defaulted))error("descriptor_dacl");
            if(!present||acl==IntPtr.Zero||owner==IntPtr.Zero)throw new InvalidOperationException("session_qualification_descriptor_parts");
            using(privilege_scope restore=new privilege_scope("SeRestorePrivilege"))
            using(privilege_scope backup=new privilege_scope("SeBackupPrivilege"))
            using(SafeFileHandle handle=open_backup(path,read_control|write_dac|write_owner)) {
                file_information info;if(!GetFileInformationByHandle(handle,out info))error("file_metadata");
                if((info.attributes&0x10)==0 && info.links!=1)throw new InvalidOperationException("session_qualification_hardlink_forbidden");
                uint code=SetSecurityInfo(handle.DangerousGetHandle(),1,0x80000005,owner,IntPtr.Zero,acl,IntPtr.Zero);
                if(code!=0)status_error("file_security_set",code);
            }
        } finally {LocalFree(sd);}
    }
    public static string read_receipt(string name) { name_ok(name);string path=Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),@"AI Agent Gateway\SessionAuthority\qualification",name,"qualification.json");using(privilege_scope privilege=new privilege_scope("SeBackupPrivilege"))using(SafeFileHandle handle=open_backup(path,0x80000000|read_control)){file_information info;if(!GetFileInformationByHandle(handle,out info))error("file_metadata");if(info.links!=1||(info.attributes&0x10)!=0)throw new InvalidOperationException("session_qualification_receipt_identity_invalid");IntPtr owner,group,acl,sacl,sd;uint code=GetSecurityInfo(handle.DangerousGetHandle(),1,1,out owner,out group,out acl,out sacl,out sd);if(code!=0)status_error("receipt_owner",code);try{if(new SecurityIdentifier(owner).Value!=service_sid_value(name))throw new InvalidOperationException("session_qualification_receipt_owner_mismatch");}finally{LocalFree(sd);}using(FileStream stream=new FileStream(handle,FileAccess.Read)){if(stream.Length>65536)throw new InvalidOperationException("session_qualification_receipt_bounds");byte[] bytes=new byte[(int)stream.Length];int read=0;while(read<bytes.Length){int count=stream.Read(bytes,read,bytes.Length-read);if(count==0)throw new InvalidOperationException("session_qualification_receipt_incomplete");read+=count;}return new UTF8Encoding(false,true).GetString(bytes);}} }
}
'@
    Add-Type -TypeDefinition $native_source -Language CSharp
}

function write_journal([string]$event) {
    $script:journal.events = @($script:journal.events) + @([ordered]@{at=[DateTime]::UtcNow.ToString('o');stage=$script:stage;event=$event})
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($script:journal | ConvertTo-Json -Depth 24))
    if ($bytes.Length -gt 4194304) { fail 'session_qualification_journal_bounds' }
    $temporary = $script:journal_path + '.new_' + [Guid]::NewGuid().ToString('N')
    [AagSessionQualificationNative]::write_new_file($temporary,$bytes,(file_sddl))
    [AagSessionQualificationNative]::replace_journal($temporary,$script:journal_path)
}

function assert_admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try { if (-not [Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { fail 'session_qualification_elevation_required' } }
    finally { $identity.Dispose() }
}

function assert_security([string]$path,[string]$sddl) {
    if (-not (same_sddl ([AagSessionQualificationNative]::read_sddl($path)) $sddl)) { fail 'session_qualification_acl_readback_failed' }
}

function acquire_lock([bool]$recover) {
    $path = $script:journal.lock_path
    if ([IO.File]::Exists($path)) {
        if (-not $recover) { fail 'session_qualification_operation_locked' }
        assert_security $path (file_sddl)
        $stream = [IO.FileStream]::new($path,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
        try { $reader=[IO.StreamReader]::new($stream,[Text.Encoding]::UTF8,$false,1024,$true);$text=$reader.ReadToEnd();$reader.Dispose();if($text -cne $script:journal.run_id){fail 'session_qualification_lock_owner_mismatch'} }
        catch { $stream.Dispose(); throw }
        $script:lock_stream=$stream
    } else {
        [AagSessionQualificationNative]::write_new_file($path,[Text.Encoding]::UTF8.GetBytes($script:journal.run_id),(file_sddl))
        $script:lock_stream=[IO.FileStream]::new($path,[IO.FileMode]::Open,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    }
    $script:lock_owned=$true
}

function release_lock {
    if ($script:lock_stream) { $script:lock_stream.Dispose();$script:lock_stream=$null }
    if ($script:lock_owned) {
        $path=$script:journal.lock_path
        no_reparse $path
        if ([IO.File]::ReadAllText($path) -cne $script:journal.run_id) { fail 'session_qualification_lock_owner_mismatch' }
        assert_security $path (file_sddl)
        [IO.File]::Delete($path)
        $script:lock_owned=$false
    }
}

function trusted_parent([string]$path) {
    no_reparse $path
    $security=[Security.AccessControl.RawSecurityDescriptor]::new([AagSessionQualificationNative]::read_sddl($path))
    if ($security.Owner.Value -notin @('S-1-5-18','S-1-5-32-544')) { fail 'session_qualification_unknown_parent_owner' }
    foreach ($ace in $security.DiscretionaryAcl) {
        if ($ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed -and
            ($ace.AceFlags -band [Security.AccessControl.AceFlags]::InheritOnly) -eq 0 -and
            $ace.SecurityIdentifier.Value -notin @('S-1-5-18','S-1-5-32-544') -and
            ($ace.AccessMask -band 0x000d0156) -ne 0) { fail 'session_qualification_untrusted_parent_writer' }
    }
}

function trusted_os_root([string]$path) {
    no_reparse $path
    $security=[Security.AccessControl.RawSecurityDescriptor]::new((Get-Acl -LiteralPath $path).Sddl)
    $trusted=@('S-1-5-18','S-1-5-32-544','S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
    if($security.Owner.Value -notin $trusted -or -not $security.DiscretionaryAcl){fail 'session_qualification_os_root_untrusted'}
    foreach($ace in $security.DiscretionaryAcl){
        if($ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed -and
            ($ace.AceFlags -band [Security.AccessControl.AceFlags]::InheritOnly) -eq 0 -and
            $ace.SecurityIdentifier.Value -notin $trusted -and ($ace.AccessMask -band 0x100d0040) -ne 0){fail 'session_qualification_os_root_replace_rights'}
    }
}

function owned_directory([string]$path,[string]$owner) {
    if ([IO.Directory]::Exists($path) -or [IO.File]::Exists($path)) { fail 'session_qualification_directory_exists' }
    $record=[ordered]@{path=$path;owner=$owner;sddl=(directory_sddl $owner);state='intent'}
    $script:journal.directories=@($script:journal.directories)+@($record)
    write_journal 'directory_create_intent'
    no_reparse ([IO.Path]::GetDirectoryName($path))
    [AagSessionQualificationNative]::create_directory($path,$record.sddl)
    assert_security $path $record.sddl
    $record.state='created';write_journal 'directory_created_verified'
}

function verify_service($record,[bool]$configured) {
    $snapshot=[AagSessionQualificationNative]::query_service($record.name)
    if (-not $snapshot) { fail 'session_qualification_owned_service_missing' }
    if ($snapshot.display -cne $record.display_tag -or $snapshot.account -ine $record.account -or $snapshot.type -ne 16 -or $snapshot.start -ne 3) {
        fail 'session_qualification_service_owner_mismatch'
    }
    $service_security=[Security.AccessControl.RawSecurityDescriptor]::new($snapshot.security)
    $writers=@('S-1-5-18','S-1-5-32-544')
    if($script:journal.services[0].expected_sid){$writers+=@($script:journal.services[0].expected_sid)}
    if(-not $service_security.DiscretionaryAcl -or $service_security.Owner.Value -notin $writers){fail 'session_qualification_service_security_invalid'}
    foreach($ace in $service_security.DiscretionaryAcl){
        if($ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed -and
            ($ace.AceFlags -band [Security.AccessControl.AceFlags]::InheritOnly) -eq 0 -and
            $ace.SecurityIdentifier.Value -notin $writers -and ($ace.AccessMask -band 0x500d0002) -ne 0){fail 'session_qualification_service_config_writer_untrusted'}
    }
    $allowed=@($record.bootstrap_image)
    if ($record.final_image) { $allowed+=@($record.final_image) }
    if ($snapshot.binary -cnotin $allowed) { fail 'session_qualification_service_image_mismatch' }
    if ($snapshot.sid_type -notin @(0,1) -or (@($snapshot.privileges).Count -gt 0 -and
        (@($snapshot.privileges | Sort-Object) -join '|') -cne (@($record.privileges | Sort-Object) -join '|'))) {
        fail 'session_qualification_service_configuration_mismatch'
    }
    if ($configured -and ($snapshot.binary -cne $record.final_image -or $snapshot.sid_type -ne 1 -or
        (@($snapshot.privileges | Sort-Object) -join '|') -cne (@($record.privileges | Sort-Object) -join '|'))) {
        fail 'session_qualification_service_configuration_mismatch'
    }
    $snapshot
}

function wait_service($record,[uint32]$state) {
    $wait_clock=[Diagnostics.Stopwatch]::StartNew()
    do {
        $snapshot=verify_service $record $true
        if ($snapshot.state -eq $state) { return $snapshot }
        if ($state -eq 4 -and $snapshot.state -eq 1) { fail 'session_qualification_service_failed_before_ready' }
        Start-Sleep -Milliseconds 100
    } while ($wait_clock.Elapsed.TotalSeconds -lt $WaitSeconds)
    fail 'session_qualification_service_wait_timeout'
}

function verify_binary {
    no_reparse $script:journal.binary
    $metadata=[AagSessionQualificationNative]::metadata($script:journal.binary)
    $digest=(Get-FileHash -LiteralPath $script:journal.binary -Algorithm SHA256).Hash.ToLowerInvariant()
    validate_binary_snapshot $metadata $digest ([AagSessionQualificationNative]::read_sddl($script:journal.binary)) $script:journal
}

function validate_binary_snapshot($metadata,[string]$digest,[string]$sddl,$plan) {
    if ($metadata.links -ne 1 -or $metadata.directory -or $metadata.owner -cne 'S-1-5-32-544') { fail 'session_qualification_artifact_identity_changed' }
    if ($plan.binary_sha256 -cnotmatch '^[0-9a-f]{64}$' -or $digest -cne $plan.binary_sha256) { fail 'session_qualification_artifact_integrity_changed' }
    if(-not (binary_acl_matches $sddl $plan)) {
        fail 'session_qualification_artifact_acl_changed'
    }
}

function binary_acl_matches([string]$actual,$plan) {
    if(-not (has_fields $plan @('binary_acl','copy_integrity','services'))){fail 'session_qualification_artifact_acl_record_invalid'}
    $state=$plan.binary_acl
    if(-not (has_fields $state @('initial_intended','initial_verified','readers_intended','readers_verified','reader_sids')) -or
        $plan.copy_integrity -isnot [bool]){fail 'session_qualification_artifact_acl_record_invalid'}
    foreach($flag in @('initial_intended','initial_verified','readers_intended','readers_verified')) {
        if($state.$flag -isnot [bool]){fail 'session_qualification_artifact_acl_record_invalid'}
    }
    if(-not $state.initial_intended -or ($state.initial_verified -and -not $plan.copy_integrity) -or
        ($plan.copy_integrity -and -not $state.initial_verified) -or
        ($state.readers_intended -and -not $state.initial_verified) -or
        ($state.readers_verified -and -not $state.readers_intended)) {fail 'session_qualification_artifact_acl_record_invalid'}
    $recorded_readers=@($state.reader_sids)
    if(-not $state.readers_intended) {
        if($recorded_readers.Count -ne 0){fail 'session_qualification_artifact_acl_record_invalid'}
        return (same_sddl $actual (file_sddl))
    }
    $service_readers=@($plan.services|ForEach-Object{$_.expected_sid})
    if($service_readers.Count -ne 3 -or $recorded_readers.Count -ne 3 -or
        @($recorded_readers|Select-Object -Unique).Count -ne 3) {fail 'session_qualification_artifact_acl_record_invalid'}
    for($index=0;$index -lt 3;$index++) {
        if($recorded_readers[$index] -isnot [string] -or
            $recorded_readers[$index] -cnotmatch '^S-1-5-80-\d+-\d+-\d+-\d+-\d+$' -or
            $recorded_readers[$index] -cne $service_readers[$index]) {fail 'session_qualification_artifact_acl_record_invalid'}
    }
    # An interrupted SetSecurityInfo may leave either exact descriptor after
    # durable intent. Once readback is journaled, only the new descriptor fits.
    (same_sddl $actual (file_sddl $recorded_readers)) -or
        (-not $state.readers_verified -and (same_sddl $actual (file_sddl)))
}

function copy_binary {
    $script:stage='artifact_copy'
    $source=[IO.FileStream]::new($script:journal.source_binary,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        if ($source.Length -lt 128 -or $source.Length -gt 2147483648 -or $source.ReadByte() -ne 77 -or $source.ReadByte() -ne 90) { fail 'session_qualification_binary_not_pe' }
        $source.Position=0;$sha=[Security.Cryptography.SHA256]::Create()
        try { $script:journal.binary_sha256=([BitConverter]::ToString($sha.ComputeHash($source))).Replace('-','').ToLowerInvariant() } finally {$sha.Dispose()}
        $script:journal.binary_acl.initial_intended=$true;write_journal 'artifact_copy_intent'
        $source.Position=0;$destination=[AagSessionQualificationNative]::create_file_stream($script:journal.binary,(file_sddl))
        try { $source.CopyTo($destination,131072);$destination.Flush($true) } finally {$destination.Dispose()}
    } finally { $source.Dispose() }
    verify_binary
    $script:journal.binary_acl.initial_verified=$true
    $script:journal.copy_integrity=$true;write_journal 'artifact_copy_verified'
}

function qualification_image($record,[string]$policy_sid) {
    $arguments=@('qualify-service','--role',$record.role,'--directory',$record.directory,
        '--expected-sid',$record.expected_sid,'--storage-sid',$policy_sid)
    if ($record.role -eq 'runtime') { $arguments+=@('--source-key',$script:journal.source_key,'--generation',([string]$script:journal.generation),'--canonical-id',$script:journal.canonical_id) }
    if ($record.role -ne 'policy' -and $script:journal.native_executable) { $arguments+=@('--native-executable',$script:journal.native_executable) }
    image_path $script:journal.binary $arguments
}

function read_qualification($record) {
    $receipt_text=[AagSessionQualificationNative]::read_receipt($record.name)
    validate_qualification $receipt_text $record $script:journal
}

function validate_qualification([string]$receipt_text,$record,$plan) {
    $receipt=$receipt_text | ConvertFrom-Json
    if(-not (has_fields $receipt @('schema_version','normal_principal_verified','policy_process_protected','service_name','service_user_sid','role',
        'result','protected_policy_threads','native_cli_version','source_key','physical_generation'))) {fail 'session_qualification_receipt_mismatch'}
    if(-not (has_fields $receipt @('native_app_server_initialized')) -or $receipt.native_app_server_initialized -isnot [bool] -or
        $receipt.native_app_server_initialized -ne ($record.role -cne 'policy' -and [bool]$plan.native_executable)) {
        fail 'session_qualification_native_initialize_mismatch'
    }
    if(-not (has_fields $receipt @('conversation_mutations_issued')) -or
        ($receipt.conversation_mutations_issued -isnot [int] -and $receipt.conversation_mutations_issued -isnot [long] -and
        $receipt.conversation_mutations_issued -isnot [System.Numerics.BigInteger]) -or $receipt.conversation_mutations_issued -ne 0) {
        fail 'session_qualification_conversation_mutation_proof_invalid'
    }
    if ($receipt.normal_principal_verified -isnot [bool] -or $receipt.policy_process_protected -isnot [bool] -or
        $receipt.service_name -isnot [string] -or $receipt.service_user_sid -isnot [string] -or $receipt.role -isnot [string] -or
        $receipt.schema_version -ne 1 -or $receipt.service_name -cne $record.name -or
        $receipt.service_user_sid -cne $record.expected_sid -or -not $receipt.normal_principal_verified -or
        $receipt.result -cne 'identity_verified_no_conversation_admitted' -or $receipt.role -cne $record.role) { fail 'session_qualification_receipt_mismatch' }
    if ($record.role -eq 'policy' -and (-not $receipt.policy_process_protected -or $receipt.protected_policy_threads -lt 1 -or $null -ne $receipt.native_cli_version)) { fail 'session_qualification_policy_receipt_invalid' }
    if ($record.role -eq 'runtime') {
        $generation_fields=[regex]::Matches($receipt_text,'"physical_generation"\s*:\s*([0-9]+)\s*[,}]')
        if($receipt.source_key -cne $plan.source_key -or $generation_fields.Count -ne 1 -or
            $generation_fields[0].Groups[1].Value -cne $plan.generation_text){fail 'session_qualification_runtime_receipt_invalid'}
    }
    if ($record.role -ne 'policy' -and $receipt.policy_process_protected) { fail 'session_qualification_foreign_policy_proof' }
    if ($record.role -ne 'policy' -and $plan.native_executable -and
        ($receipt.native_cli_version -isnot [string] -or $receipt.native_cli_version.Length -gt 128 -or
        $receipt.native_cli_version -notmatch '^codex-cli [A-Za-z0-9 ._+-]+$')) { fail 'session_qualification_native_version_missing' }
    [ordered]@{service_name=$receipt.service_name;role=$receipt.role;actual_user_sid=$receipt.service_user_sid;
        normal_principal_verified=$true;policy_process_protected=[bool]$receipt.policy_process_protected;
        native_app_server_initialized=$receipt.native_app_server_initialized;conversation_mutations_issued=$receipt.conversation_mutations_issued;
        protected_policy_threads=$receipt.protected_policy_threads;native_cli_version=$receipt.native_cli_version}
}

function has_fields($value,[string[]]$names) {
    if($null -eq $value){return $false}
    foreach($name in $names) {
        if($value -is [Collections.IDictionary]){if(-not $value.Contains($name)){return $false}}
        elseif($name -cnotin $value.PSObject.Properties.Name){return $false}
    }
    $true
}

function fixture_assert([bool]$condition) {
    if(-not $condition){fail 'session_qualification_self_test_assertion'}
}

function fixture_rejected([scriptblock]$action,[string]$expected_code) {
    try { $null=& $action } catch {
        if($_.Exception.Message -ceq $expected_code){return}
        throw
    }
    fail 'session_qualification_self_test_expected_rejection'
}

function qualification_fixture([string]$role,[bool]$native) {
    $services=@(
        [pscustomobject]@{name='aag_session_authority';role='policy';expected_sid='S-1-5-80-1-2-3-4-5'},
        [pscustomobject]@{name='aag_session_reader';role='reader';expected_sid='S-1-5-80-1-2-3-4-6'},
        [pscustomobject]@{name='aag_session_runtime_0123456789abcdef0123456789abcdef_g7';role='runtime';expected_sid='S-1-5-80-1-2-3-4-7'}
    )
    $record=@($services|Where-Object{$_.role -ceq $role})[0]
    $plan=[pscustomobject]@{services=$services;native_executable=$(if($native){'synthetic-native.exe'}else{$null});
        source_key='0123456789abcdef0123456789abcdef';generation_text='7';copy_integrity=$true;binary_sha256=('a'*64);
        binary_acl=[pscustomobject]@{initial_intended=$true;initial_verified=$true;readers_intended=$false;readers_verified=$false;reader_sids=@()}}
    $receipt=[ordered]@{schema_version=1;service_name=$record.name;service_user_sid=$record.expected_sid;role=$role;
        source_key=$(if($role -ceq 'runtime'){$plan.source_key}else{$null});physical_generation=$(if($role -ceq 'runtime'){7}else{$null});
        normal_principal_verified=$true;policy_process_protected=($role -ceq 'policy');
        protected_policy_threads=$(if($role -ceq 'policy'){2}else{$null});
        native_cli_version=$(if($native -and $role -cne 'policy'){'codex-cli 0.0.0'}else{$null});
        native_app_server_initialized=($native -and $role -cne 'policy');conversation_mutations_issued=0;
        result='identity_verified_no_conversation_admitted'}
    [pscustomobject]@{record=$record;plan=$plan;receipt=$receipt}
}

function run_pure_self_tests {
    # These fixtures use only in-memory JSON/security descriptors. They never
    # load the native adapter or inspect paths, services, accounts, or tokens.
    $initial_acl='O:BAD:P(A;;FA;;;BA)(A;;FA;;;SY)'
    $reader_acl='O:BAD:P(A;;FA;;;BA)(A;;FA;;;SY)(A;;0x1200a9;;;S-1-5-80-1-2-3-4-5)(A;;0x1200a9;;;S-1-5-80-1-2-3-4-6)(A;;0x1200a9;;;S-1-5-80-1-2-3-4-7)'
    $partial_acl='O:BAD:P(A;;FA;;;BA)(A;;FA;;;SY)(A;;0x1200a9;;;S-1-5-80-1-2-3-4-5)'
    $cases=@(
        @{name='qualified_receipts_retain_initialization_and_zero_mutation_proof';body={
            foreach($role in @('policy','reader','runtime')) {
                foreach($native in @($false,$true)) {
                    $fixture=qualification_fixture $role $native
                    $accepted=validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan
                    fixture_assert ($accepted.native_app_server_initialized -is [bool] -and
                        $accepted.native_app_server_initialized -eq ($native -and $role -cne 'policy'))
                    fixture_assert ($accepted.conversation_mutations_issued -is [long] -and $accepted.conversation_mutations_issued -eq 0)
                    fixture_assert ($accepted.actual_user_sid -ceq $fixture.record.expected_sid)
                }
            }
        }},
        @{name='native_receipt_requires_actual_initialize';body={
            foreach($role in @('reader','runtime')) {
                $fixture=qualification_fixture $role $true
                $fixture.receipt.native_app_server_initialized=$false
                fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_native_initialize_mismatch'
            }
        }},
        @{name='initialization_proof_must_be_present_and_boolean';body={
            foreach($invalid in @($null,'true',1)) {
                $fixture=qualification_fixture 'reader' $true
                $fixture.receipt.native_app_server_initialized=$invalid
                fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_native_initialize_mismatch'
            }
            $fixture=qualification_fixture 'reader' $true;$fixture.receipt.Remove('native_app_server_initialized')
            fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_native_initialize_mismatch'
        }},
        @{name='policy_and_no_native_receipts_require_false_initialize';body={
            foreach($role in @('policy','reader','runtime')) {
                $fixture=qualification_fixture $role $false;$fixture.receipt.native_app_server_initialized=$true
                fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_native_initialize_mismatch'
            }
            $fixture=qualification_fixture 'policy' $true;$fixture.receipt.native_app_server_initialized=$true
            fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_native_initialize_mismatch'
        }},
        @{name='mutation_proof_rejects_missing_wrong_types_and_nonzero';body={
            foreach($invalid in @($null,$false,'0',0.0,1,-1)) {
                $fixture=qualification_fixture 'runtime' $true;$fixture.receipt.conversation_mutations_issued=$invalid
                fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_conversation_mutation_proof_invalid'
            }
            $fixture=qualification_fixture 'runtime' $true;$fixture.receipt.Remove('conversation_mutations_issued')
            fixture_rejected {validate_qualification ($fixture.receipt|ConvertTo-Json) $fixture.record $fixture.plan} 'session_qualification_conversation_mutation_proof_invalid'
        }},
        @{name='initial_binary_acl_survives_partial_sid_discovery';body={
            $fixture=qualification_fixture 'policy' $false
            $fixture.plan.services[1].expected_sid=$null;$fixture.plan.services[2].expected_sid=$null
            # Simulate the durable journal after the first SID lookup and
            # before any binary ACL transition, then reload as rollback does.
            $persisted=$fixture.plan|ConvertTo-Json -Depth 10|ConvertFrom-Json
            validate_binary_snapshot ([pscustomobject]@{owner='S-1-5-32-544';links=1;directory=$false}) ('a'*64) $initial_acl $persisted
            fixture_assert (-not (binary_acl_matches $partial_acl $persisted))
        }},
        @{name='interrupted_binary_acl_transition_accepts_only_exact_pre_or_post';body={
            $fixture=qualification_fixture 'reader' $false
            $fixture.plan.binary_acl.reader_sids=@('S-1-5-80-1-2-3-4-5','S-1-5-80-1-2-3-4-6','S-1-5-80-1-2-3-4-7')
            $fixture.plan.binary_acl.readers_intended=$true
            $persisted=$fixture.plan|ConvertTo-Json -Depth 10|ConvertFrom-Json
            fixture_assert (binary_acl_matches $initial_acl $persisted)
            fixture_assert (binary_acl_matches $reader_acl $persisted)
            fixture_assert (-not (binary_acl_matches $partial_acl $persisted))
        }},
        @{name='verified_binary_acl_rejects_prior_descriptor_and_unrecorded_grants';body={
            $fixture=qualification_fixture 'reader' $false
            fixture_assert (-not (binary_acl_matches $reader_acl $fixture.plan))
            $fixture.plan.binary_acl.reader_sids=@('S-1-5-80-1-2-3-4-5','S-1-5-80-1-2-3-4-6','S-1-5-80-1-2-3-4-7')
            $fixture.plan.binary_acl.readers_intended=$true;$fixture.plan.binary_acl.readers_verified=$true
            $persisted=$fixture.plan|ConvertTo-Json -Depth 10|ConvertFrom-Json
            fixture_assert (binary_acl_matches $reader_acl $persisted)
            fixture_assert (-not (binary_acl_matches $initial_acl $persisted))
            fixture_assert (-not (binary_acl_matches ($reader_acl+'(A;;FA;;;BU)') $persisted))
        }},
        @{name='unjustified_or_inconsistent_acl_journal_cannot_authorize_cleanup';body={
            foreach($injection in @('no_copy_intent','reader_verified_without_intent','wrong_reader_identity','missing_fields','copy_not_verified')) {
                $fixture=qualification_fixture 'reader' $false
                switch($injection) {
                    'no_copy_intent' {$fixture.plan.binary_acl.initial_intended=$false}
                    'reader_verified_without_intent' {$fixture.plan.binary_acl.readers_verified=$true}
                    'wrong_reader_identity' {$fixture.plan.binary_acl.readers_intended=$true;$fixture.plan.binary_acl.reader_sids=@('S-1-5-80-1-2-3-4-5','S-1-5-80-1-2-3-4-6','S-1-5-80-1-2-3-4-8')}
                    'missing_fields' {$fixture.plan.binary_acl=[pscustomobject]@{initial_intended=$true}}
                    'copy_not_verified' {$fixture.plan.binary_acl.readers_intended=$true;$fixture.plan.copy_integrity=$false}
                }
                fixture_rejected {binary_acl_matches $initial_acl $fixture.plan} 'session_qualification_artifact_acl_record_invalid'
            }
        }},
        @{name='artifact_acl_match_never_bypasses_owner_link_or_digest';body={
            $fixture=qualification_fixture 'policy' $false
            foreach($metadata in @(
                [pscustomobject]@{owner='S-1-5-80-1-2-3-4-5';links=1;directory=$false},
                [pscustomobject]@{owner='S-1-5-32-544';links=2;directory=$false},
                [pscustomobject]@{owner='S-1-5-32-544';links=1;directory=$true}
            )) {fixture_rejected {validate_binary_snapshot $metadata ('a'*64) $initial_acl $fixture.plan} 'session_qualification_artifact_identity_changed'}
            $metadata=[pscustomobject]@{owner='S-1-5-32-544';links=1;directory=$false}
            fixture_rejected {validate_binary_snapshot $metadata ('b'*64) $initial_acl $fixture.plan} 'session_qualification_artifact_integrity_changed'
        }},
        @{name='completed_copy_before_readback_can_be_reconciled_with_digest';body={
            $fixture=qualification_fixture 'policy' $false
            $fixture.plan.binary_acl.initial_verified=$false;$fixture.plan.copy_integrity=$false
            validate_binary_snapshot ([pscustomobject]@{owner='S-1-5-32-544';links=1;directory=$false}) ('a'*64) $initial_acl $fixture.plan
            fixture_rejected {validate_binary_snapshot ([pscustomobject]@{owner='S-1-5-32-544';links=1;directory=$false}) ('b'*64) $initial_acl $fixture.plan} 'session_qualification_artifact_integrity_changed'
        }}
    )
    $results=@(foreach($case in $cases){
        try{& $case.body;[ordered]@{name=$case.name;passed=$true}}
        catch{[ordered]@{name=$case.name;passed=$false;code=$_.Exception.Message}}
    })
    $failed=@($results|Where-Object{-not $_.passed}).Count
    [ordered]@{mode='self_test';mutations=$false;native_adapter_loaded=[bool]('AagSessionQualificationNative' -as [type]);passed=($results.Count-$failed);failed=$failed;cases=$results}|ConvertTo-Json -Depth 6
    if($failed -gt 0){exit 1}
}

function assert_native_protected {
    if (-not $script:journal.native_executable) { return }
    $cursor = $script:journal.native_executable
    $trusted = @('S-1-5-18','S-1-5-32-544','S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
    while (within_path $cursor $script:journal.program_files) {
        no_reparse $cursor
        $acl = Get-Acl -LiteralPath $cursor
        $raw = [Security.AccessControl.RawSecurityDescriptor]::new($acl.Sddl)
        if ($raw.Owner.Value -notin $trusted -or -not $raw.DiscretionaryAcl) { fail 'session_qualification_native_owner_untrusted' }
        foreach ($ace in $raw.DiscretionaryAcl) {
            if ($ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed -and
                ($ace.AceFlags -band [Security.AccessControl.AceFlags]::InheritOnly) -eq 0 -and
                $ace.SecurityIdentifier.Value -notin $trusted -and ($ace.AccessMask -band 0x500d0156) -ne 0) {
                fail 'session_qualification_native_writer_untrusted'
            }
        }
        if ($cursor -ieq $script:journal.program_files) { break }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
}

function apply_plan {
    $script:stage='preflight'
    if (-not [IO.File]::Exists($script:journal.source_binary)) { fail 'session_qualification_source_binary_missing' }
    if ($script:journal.native_executable -and -not [IO.File]::Exists($script:journal.native_executable)) { fail 'session_qualification_native_binary_missing' }
    assert_native_protected
    trusted_os_root $script:journal.program_files
    trusted_os_root $script:journal.program_data
    foreach ($root in @($script:journal.artifact_root,$script:journal.data_root)) { no_reparse $root;if(Test-Path -LiteralPath $root){fail 'session_qualification_unknown_existing_root'} }
    foreach ($service in $script:journal.services) { if([AagSessionQualificationNative]::query_service($service.name)){fail 'session_qualification_existing_service_forbidden'} }
    foreach ($history_file in Get-ChildItem -LiteralPath $script:journal.program_data -Filter 'aag_session_qualification_*.json' -File) {
        assert_security $history_file.FullName (file_sddl)
        $history=[IO.File]::ReadAllText($history_file.FullName) | ConvertFrom-Json
        if ($history.owner -cne $script:marker -or $history.schema_version -ne 1 -or $history.generation_text -cnotmatch '^[1-9][0-9]{0,19}$') { fail 'session_qualification_unknown_journal' }
        if ($history.source_key -ceq $script:journal.source_key -and $history.generation_text -ceq $script:journal.generation_text) { fail 'session_qualification_generation_already_spent' }
    }
    foreach ($parent in @([IO.Path]::GetDirectoryName($script:journal.artifact_root),[IO.Path]::GetDirectoryName($script:journal.data_root))) { if([IO.Directory]::Exists($parent)){trusted_parent $parent} }
    $script:journal.phase='applying'
    [AagSessionQualificationNative]::write_new_file($script:journal_path,[Text.UTF8Encoding]::new($false).GetBytes(($script:journal|ConvertTo-Json -Depth 24)),(file_sddl))
    acquire_lock $false
    foreach ($parent in @([IO.Path]::GetDirectoryName($script:journal.artifact_root),[IO.Path]::GetDirectoryName($script:journal.data_root))) { if(-not [IO.Directory]::Exists($parent)){owned_directory $parent 'S-1-5-32-544'} }
    owned_directory $script:journal.artifact_root 'S-1-5-32-544'
    owned_directory $script:journal.data_root 'S-1-5-32-544'
    owned_directory $script:journal.qualification_root 'S-1-5-32-544'
    copy_binary
    $script:stage='services_create'
    foreach ($service in $script:journal.services) {
        $service.create_state='intent';write_journal 'service_create_intent'
        [AagSessionQualificationNative]::create_service($service.name,$service.display_tag,$service.bootstrap_image)
        $service.create_state='created';$null=verify_service $service $false
        $service.expected_sid=[AagSessionQualificationNative]::service_sid_value($service.name)
        if($service.expected_sid -notmatch '^S-1-5-80-\d+-\d+-\d+-\d+-\d+$'){fail 'session_qualification_virtual_user_sid_invalid'}
        write_journal 'service_created_sid_resolved'
    }
    $sid_values=@($script:journal.services | ForEach-Object {$_.expected_sid})
    if(@($sid_values|Select-Object -Unique).Count -ne 3){fail 'session_qualification_principals_not_distinct'}
    foreach($root in @($script:journal.artifact_root,$script:journal.data_root,$script:journal.qualification_root)) {
        $sddl=directory_sddl 'S-1-5-32-544' $sid_values
        write_journal 'root_reader_acl_intent';[AagSessionQualificationNative]::apply_sddl($root,$sddl);assert_security $root $sddl
    }
    $binary_sddl=file_sddl $sid_values
    $script:journal.binary_acl.reader_sids=@($sid_values);$script:journal.binary_acl.readers_intended=$true
    write_journal 'binary_reader_acl_intent'
    [AagSessionQualificationNative]::apply_sddl($script:journal.binary,$binary_sddl);assert_security $script:journal.binary $binary_sddl
    $script:journal.binary_acl.readers_verified=$true;write_journal 'binary_reader_acl_verified'
    foreach($service in $script:journal.services){owned_directory $service.directory $service.expected_sid}
    $policy_sid=$script:journal.services[0].expected_sid
    $script:stage='policy_rights'
    $script:journal.policy_rights.sid=$policy_sid
    $script:journal.policy_rights.before=@([AagSessionQualificationNative]::policy_assigned_rights($policy_sid))
    $script:journal.policy_rights.added=@($script:policy_rights|Where-Object{$_ -notin $script:journal.policy_rights.before})
    $script:journal.policy_rights.intent=$true;write_journal 'policy_rights_add_intent'
    [AagSessionQualificationNative]::change_policy_rights($policy_sid,[string[]]$script:journal.policy_rights.added,$true)
    if((@([AagSessionQualificationNative]::policy_assigned_rights($policy_sid)|Sort-Object)-join '|') -cne (@($script:policy_rights|Sort-Object)-join '|')){fail 'session_qualification_policy_rights_readback_failed'}
    write_journal 'policy_rights_verified'
    $script:stage='services_configure'
    foreach($service in $script:journal.services){
        $service.final_image=qualification_image $service $policy_sid;write_journal 'service_configure_intent'
        [AagSessionQualificationNative]::protect_service($service.name,$policy_sid)
        [AagSessionQualificationNative]::configure_service($service.name,$service.final_image,[string[]]$service.privileges)
        $null=verify_service $service $true;$service.configured=$true;write_journal 'service_configuration_verified'
    }
    $script:stage='services_start'
    foreach($service in $script:journal.services){
        verify_binary;$null=verify_service $service $true;write_journal 'service_start_intent'
        [AagSessionQualificationNative]::control_service($service.name,'start')
        $status=wait_service $service 4;$service.started=$true;$service.process_id=$status.pid
        $script:journal.receipts=@($script:journal.receipts)+@(read_qualification $service)
        write_journal 'service_running_actual_token_receipt_verified'
    }
    $script:journal.phase='applied';write_journal 'qualification_applied'
}

function load_rollback {
    if(-not $ManifestPath){fail 'session_qualification_manifest_required'}
    $path=plain_path $ManifestPath
    $pd=plain_path ([Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData))
    if([IO.Path]::GetDirectoryName($path) -ine $pd -or [IO.Path]::GetFileName($path) -cnotmatch '^aag_session_qualification_([0-9a-f]{32})\.json$'){fail 'session_qualification_manifest_scope_invalid'}
    $manifest_run_id=$Matches[1]
    no_reparse $path
    $info=[IO.FileInfo]::new($path);if(-not $info.Exists -or $info.Length -gt 4194304){fail 'session_qualification_manifest_invalid'}
    $saved=[IO.File]::ReadAllText($path)|ConvertFrom-Json
    if($saved.schema_version -ne 1 -or $saved.owner -cne $script:marker -or $saved.run_id -cne $manifest_run_id){fail 'session_qualification_manifest_invalid'}
    $script:journal=$saved;$script:journal_path=$path
    # Recompute every resource path from fixed roots and validated identity.
    if($saved.generation_text -cnotmatch '^[1-9][0-9]{0,19}$'){fail 'session_qualification_manifest_generation_invalid'}
    $parsed_generation=[uint64]0
    if(-not [uint64]::TryParse($saved.generation_text,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture,[ref]$parsed_generation)){fail 'session_qualification_manifest_generation_invalid'}
    $saved.generation=$parsed_generation
    $script:RunId=$saved.run_id;$script:SourceKey=$saved.source_key;$script:Generation=$parsed_generation
    $script:CanonicalId=$saved.canonical_id;$script:SourceBinary=$saved.source_binary;$script:NativeExecutable=$saved.native_executable
    $expected=build_plan
    foreach($field in @('artifact_root','data_root','qualification_root','binary','manifest_path','lock_path','program_data','program_files')){if($saved.$field -cne $expected.$field){fail 'session_qualification_manifest_path_mismatch'}}
    if(@($saved.services).Count -ne 3){fail 'session_qualification_manifest_services_invalid'}
    for($i=0;$i -lt 3;$i++) { foreach($field in @('name','role','account','directory','display_tag','bootstrap_image')){if($saved.services[$i].$field -cne $expected.services[$i].$field){fail 'session_qualification_manifest_service_mismatch'}} }
    $saved
}

function remove_owned_tree($record) {
    if(-not [IO.Directory]::Exists($record.directory)){return}
    $owned_record=@($script:journal.directories|Where-Object{$_.path -ceq $record.directory -and $_.owner -ceq $record.expected_sid -and $_.state -in @('intent','created')})
    if($owned_record.Count -ne 1){fail 'session_qualification_unowned_tree'}
    $root=plain_path $record.directory
    if(-not (within_path $root $script:journal.qualification_root) -or $root -eq $script:journal.qualification_root){fail 'session_qualification_delete_scope_invalid'}
    $stack=[Collections.Generic.Stack[string]]::new();$stack.Push($root);$paths=[Collections.Generic.List[object]]::new()
    while($stack.Count -gt 0){
        $path=$stack.Pop();no_reparse $path
        if(-not (within_path (plain_path $path) $root)){fail 'session_qualification_delete_scope_invalid'}
        $meta=[AagSessionQualificationNative]::metadata($path)
        if($meta.owner -cne $record.expected_sid -or (-not $meta.directory -and $meta.links -ne 1)){fail 'session_qualification_unknown_tree_owner'}
        $sddl=if($meta.directory){directory_sddl $record.expected_sid}else{"O:$($record.expected_sid)D:P(A;;FA;;;$($record.expected_sid))(A;;FA;;;BA)(A;;FA;;;SY)"}
        [AagSessionQualificationNative]::apply_sddl($path,$sddl)
        assert_security $path $sddl
        $paths.Add([pscustomobject]@{path=$path;directory=$meta.directory})
        if($paths.Count -gt $script:max_tree_entries){fail 'session_qualification_tree_bounds'}
        if($meta.directory){foreach($child in Get-ChildItem -LiteralPath $path -Force){$stack.Push($child.FullName)}}
    }
    # Entire tree is scanned before any deletion; never follow reparse points
    # or alter a hard-linked file's security outside this fresh owned tree.
    write_journal 'owned_tree_delete_intent'
    foreach($entry in $paths | Sort-Object @{Expression={$_.path.Length};Descending=$true}){
        no_reparse $entry.path
        if(-not (within_path (plain_path $entry.path) $root)){fail 'session_qualification_delete_scope_invalid'}
        if($entry.directory){[IO.Directory]::Delete($entry.path,$false)}else{[IO.File]::Delete($entry.path)}
    }
    write_journal 'owned_tree_removed'
}

function rollback_plan {
    assert_security $script:journal_path (file_sddl)
    acquire_lock $true
    $script:journal.phase='rollback_in_progress';write_journal 'rollback_intent'
    $script:stage='rollback_stop'
    foreach($record in @($script:journal.services)[2..0]){
        $snapshot=[AagSessionQualificationNative]::query_service($record.name)
        if(-not $snapshot){continue}
        if($record.create_state -notin @('intent','created')){fail 'session_qualification_unowned_service'}
        $snapshot=verify_service $record ([bool]$record.configured)
        if($snapshot.state -eq 1 -and $record.process_id -gt 0){
            # Read-only process presence check; never terminate a PID directly.
            $remaining_process=Get-Process -Id $record.process_id -ErrorAction SilentlyContinue
            if($remaining_process){
                try {
                    if((plain_path $remaining_process.Path) -ieq $script:journal.binary){
                        $terminal_lease=[AagSessionQualificationNative]::lease_process($record.process_id,$script:journal.binary)
                        try{if(-not $terminal_lease.wait_exit([uint32]($WaitSeconds*1000))){fail 'session_qualification_service_process_not_terminal'}}finally{$terminal_lease.Dispose()}
                    }
                } finally {$remaining_process.Dispose()}
            }
        }
        if($snapshot.state -ne 1){
            verify_binary
            if($snapshot.pid -eq 0){fail 'session_qualification_service_process_unknown'}
            $lease=[AagSessionQualificationNative]::lease_process($snapshot.pid,$script:journal.binary)
            try {
                $stop_clock=[Diagnostics.Stopwatch]::StartNew()
                while($snapshot.state -eq 2){
                    if($stop_clock.Elapsed.TotalSeconds -ge $WaitSeconds){fail 'session_qualification_service_wait_timeout'}
                    Start-Sleep -Milliseconds 100;$snapshot=verify_service $record ([bool]$record.configured)
                }
                if($snapshot.state -eq 4){write_journal 'owned_service_stop_intent';[AagSessionQualificationNative]::control_service($record.name,'stop')}
                elseif($snapshot.state -notin @(1,3)){fail 'session_qualification_service_state_unknown'}
                if($snapshot.state -ne 1){$null=wait_service $record 1}
                if(-not $lease.wait_exit([uint32]($WaitSeconds*1000))){fail 'session_qualification_service_process_not_terminal'}
                write_journal 'owned_service_stop_and_process_exit_verified'
            } finally {$lease.Dispose()}
        }
    }
    $script:stage='rollback_rights'
    if($script:journal.policy_rights.intent -and -not $script:journal.policy_rights.restored){
        $null=verify_service $script:journal.services[0] ([bool]$script:journal.services[0].configured)
        $current_rights=@([AagSessionQualificationNative]::policy_assigned_rights($script:journal.policy_rights.sid))
        $owned_union=@((@($script:journal.policy_rights.before)+@($script:journal.policy_rights.added))|Sort-Object -Unique)
        if(@($script:journal.policy_rights.before|Where-Object{$_ -notin $current_rights}).Count -gt 0 -or
            @($current_rights|Where-Object{$_ -notin $owned_union}).Count -gt 0){fail 'session_qualification_policy_rights_changed'}
        $remaining_rights=@($script:journal.policy_rights.added|Where-Object{$_ -in $current_rights})
        write_journal 'policy_rights_restore_intent'
        if($remaining_rights.Count -gt 0){[AagSessionQualificationNative]::change_policy_rights($script:journal.policy_rights.sid,[string[]]$remaining_rights,$false)}
        if((@([AagSessionQualificationNative]::policy_assigned_rights($script:journal.policy_rights.sid)|Sort-Object)-join '|') -cne (@($script:journal.policy_rights.before|Sort-Object)-join '|')){fail 'session_qualification_policy_rights_restore_failed'}
        $script:journal.policy_rights.restored=$true;write_journal 'policy_rights_restored'
    }
    $script:stage='rollback_delete_services'
    foreach($record in @($script:journal.services)[2..0]){
        $snapshot=[AagSessionQualificationNative]::query_service($record.name)
        if(-not $snapshot){continue}
        $snapshot=verify_service $record ([bool]$record.configured)
        if($snapshot.state -ne 1){fail 'session_qualification_service_not_stopped'}
        write_journal 'owned_service_delete_intent'
        [AagSessionQualificationNative]::control_service($record.name,'delete')
        $delete_clock=[Diagnostics.Stopwatch]::StartNew()
        while([AagSessionQualificationNative]::service_present($record.name)) {
            if($delete_clock.Elapsed.TotalSeconds -ge $WaitSeconds){fail 'session_qualification_service_delete_unconfirmed'}
            Start-Sleep -Milliseconds 100
        }
        $record.removed=$true;write_journal 'owned_service_deleted'
    }
    $script:stage='rollback_artifacts'
    foreach($record in $script:journal.services){if($record.expected_sid){remove_owned_tree $record}}
    if([IO.File]::Exists($script:journal.binary)){
        # Even an interrupted copy must match the source digest before removal.
        # A truncated/unrecognized artifact remains for explicit owner review.
        verify_binary
        write_journal 'owned_binary_delete_intent';[IO.File]::Delete($script:journal.binary);write_journal 'owned_binary_deleted'
    }
    foreach($directory in @($script:journal.directories)|Sort-Object @{Expression={$_.path.Length};Descending=$true}){
        if(-not [IO.Directory]::Exists($directory.path)){continue}
        no_reparse $directory.path
        $allowed=@($script:journal.artifact_root,$script:journal.data_root,$script:journal.qualification_root,
            [IO.Path]::GetDirectoryName($script:journal.artifact_root),[IO.Path]::GetDirectoryName($script:journal.data_root))
        if($directory.path -notin $allowed){continue}
        $meta=[AagSessionQualificationNative]::metadata($directory.path)
        if($meta.owner -cne $directory.owner){fail 'session_qualification_directory_owner_changed'}
        write_journal 'owned_empty_directory_delete_intent'
        [IO.Directory]::Delete($directory.path,$false)
    }
    $script:journal.phase='rolled_back';$script:journal.rollback_complete=$true;write_journal 'rollback_verified_generation_remains_spent'
}

try {
    if($SelfTest){
        if($Apply -or $Rollback){fail 'session_qualification_self_test_mode_conflict'}
        run_pure_self_tests
        return
    }
    if($Apply){assert_admin}
    if($Rollback){$plan=load_rollback}else{$plan=build_plan;$script:journal=$plan;$script:journal_path=$plan.manifest_path}
    if(-not $Apply){public_plan $plan $(if($Rollback){'rollback_plan'}else{'plan'})|ConvertTo-Json -Depth 12;return}
    if(-not $Rollback -and (-not $PSBoundParameters.ContainsKey('RunId') -or -not $PSBoundParameters.ContainsKey('SourceKey'))){fail 'session_qualification_apply_requires_reviewed_identity'}
    $script:stage='native_adapter'
    load_native_adapter
    if($Rollback){rollback_plan}else{apply_plan}
    release_lock
    [ordered]@{mode=$(if($Rollback){'rollback'}else{'apply'});ok=$true;manifest_path=$script:journal_path;
        run_id=$script:journal.run_id;phase=$script:journal.phase;copy_integrity_verified=[bool]$script:journal.copy_integrity;
        receipts=@($script:journal.receipts);runtime_generation_reuse_forbidden=$true}|ConvertTo-Json -Depth 10
} catch {
    $failure=$_.Exception
    for($inner_index=0;$inner_index -lt 8 -and $failure.InnerException;$inner_index++){$failure=$failure.InnerException}
    $message=$failure.Message
    $code=if($message -cmatch '^session_qualification_[a-z0-9_]+$'){$message}else{'session_qualification_failed'}
    if($Apply -and $script:journal -and $script:lock_owned){
        try{$script:journal.phase='failed_requires_explicit_review';write_journal $code}catch{}
    }
    try{release_lock}catch{}
    [ordered]@{ok=$false;mode=$(if($Rollback){'rollback'}elseif($Apply){'apply'}else{'plan'});code=$code;
        stage=$script:stage;manifest_path=$script:journal_path;automatic_retry=$false}|ConvertTo-Json -Depth 5
    exit 1
}
