# Reads the local API key stored by Multi LLM from Windows Credential Manager.
# Prints credential metadata always; the secret itself only with -Reveal.
param([switch]$Reveal)
$ErrorActionPreference = "Continue"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class CredMan {
  [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  public static extern bool CredEnumerate(string filter, int flag, out int count, out IntPtr pCredentials);
  [DllImport("advapi32.dll", SetLastError = true)]
  public static extern void CredFree(IntPtr buffer);
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  public struct CREDENTIAL {
    public int Flags; public int Type; public string TargetName; public string Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public int CredentialBlobSize; public IntPtr CredentialBlob; public int Persist;
    public int AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName;
  }
}
"@

[int]$count = 0
[IntPtr]$ptr = [IntPtr]::Zero
$allOk = [CredMan]::CredEnumerate("local-api-key*", 0, [ref]$count, [ref]$ptr)
if (-not $allOk) { Write-Output "NO-CREDENTIALS"; exit }
$keyFound = $null
for ($i = 0; $i -lt $count; $i++) {
  $credPtr = [Runtime.InteropServices.Marshal]::ReadIntPtr($ptr, $i * [IntPtr]::Size)
  $cred = [Runtime.InteropServices.Marshal]::PtrToStructure($credPtr, [type][CredMan+CREDENTIAL])
  $pw = ""
  if ($cred.CredentialBlobSize -gt 0) {
    $bytes = New-Object byte[] $cred.CredentialBlobSize
    [Runtime.InteropServices.Marshal]::Copy($cred.CredentialBlob, $bytes, 0, $cred.CredentialBlobSize)
    $pw = [Text.Encoding]::Unicode.GetString($bytes)
  }
  Write-Output ("credential: " + $cred.TargetName + " (user=" + $cred.UserName + ") len=" + $pw.Length)
  if ($cred.TargetName -match '^local-api-key\.MultiLLM$') { $keyFound = $pw }
}
[CredMan]::CredFree($ptr)
if ($keyFound) {
  if ($Reveal) {
    Write-Output ("LOCALKEY=" + $keyFound)
  } else {
    Write-Output "LOCALKEY-FOUND (use -Reveal to print the secret)"
  }
} else {
  Write-Output "LOCALKEY-NOT-FOUND"
}