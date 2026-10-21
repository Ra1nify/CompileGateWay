# End-to-end fixture for controlled file read (`cgw cat` / `GET /api/projects/{id}/file`) (design doc §7.4).
#
# §7.4 says the file tree "does not return file content by default; add a controlled
# read/download API later if text files need viewing". This fixture checks that
# "controlled": **boundaries** (escape / symlink / directory),
# **cap** (large file truncated and stated clearly), **binary** (metadata only, no content in the JSON).
#
# Host side, fully offline.
#
# Usage: cd src-tauri; pwsh -NoProfile -File fixtures\e2e-cat.ps1

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'cgw-cat'
$rel = 'F:\CODEX\CompileGateWay\src-tauri\target\release'

Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $root, "$root\home" | Out-Null
Copy-Item "$rel\compilegateway-daemon.exe" $root
Copy-Item "$rel\compilegateway-cli.exe" $root
Set-Content "$root\config.json" '{ "port": 0, "language": "zh-Hans", "listen": "lan", "logRetainDays": 7 }' -Encoding utf8

$env:USERPROFILE = "$root\home"
$daemon = Start-Process -FilePath "$root\compilegateway-daemon.exe" -WorkingDirectory $root -PassThru -WindowStyle Hidden
$url = $null
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Milliseconds 250
  if (Test-Path "$root\runtime.json") {
    try { $url = (Get-Content "$root\runtime.json" -Raw | ConvertFrom-Json).url } catch { }
    if ($url) { break }
  }
}
if (-not $url) { throw 'daemon did not start' }
$cgw = "$root\compilegateway-cli.exe"
Write-Host "daemon: $url"

$bad = 0
function Expect($cond, $what, $detail) { if ($cond) { Write-Host "  ok   $what" } else { Write-Host "  FAIL $what  $detail"; $script:bad++ } }

& $cgw clients add --host 127.0.0.1 --port 1 --user x --password y --name offline --url $url | Out-Null
$clients = (& $cgw clients list --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
# Design doc §6.1: browse / register project / detect build type **share one root allowlist**.
# Acceptance note §7 caught exactly the "browse says no, register says yes" case -- now both agree:
# the temp dir is not in the allowlist, so registering **is rejected** and the error says what to do.
New-Item -ItemType Directory -Force -Path "$root\src" | Out-Null
$denied = (& $cgw projects add --client $clients.clients[0].id --path "$root\src" --name 被拒的 --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -ne 0 -and $denied -match 'allowedRoots') 'a dir outside the allowlist is rejected (same check as browse)' $denied
& $cgw config set --allowed-roots "$root" --url $url | Out-Null
& $cgw projects add --client $clients.clients[0].id --path "$root\src" --name 读文件演示 --url $url | Out-Null
$projects = (& $cgw projects --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
$projectId = $projects.projects[0].id

$hostRoot = "$root\home\CompileGateway\读文件演示"
New-Item -ItemType Directory -Force -Path "$hostRoot\src\sub" | Out-Null
Set-Content "$hostRoot\src\hello.txt" -Value "第一行`n第二行中文`n" -NoNewline
Set-Content "$hostRoot\src\sub\note.md" -Value '# 标题' -NoNewline
# Large file: above the cap (256 KB), used to check truncation. **Must be text** -- all-zero bytes are judged binary
[System.IO.File]::WriteAllText("$hostRoot\src\big.txt", ('a' * 400000))
# Real binary: PNG magic bytes + a few zero bytes
[System.IO.File]::WriteAllBytes("$hostRoot\src\image.png", [byte[]](0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,0x00,0x00,0x00,0x0d,0x49,0x48,0x44,0x52))
# Symlink pointing outside the project root (needs Developer Mode on Windows; skip this case if it cannot be created)
$linkMade = $false
try {
  New-Item -ItemType SymbolicLink -Path "$hostRoot\src\escape.txt" -Target "$root\config.json" -ErrorAction Stop | Out-Null
  $linkMade = $true
} catch { }

# Careful not to name this function `Cat` -- that is the PowerShell alias for Get-Content (same trap as $host / $home).
function ReadCgw([string[]]$extra) {
  $out = (& $cgw cat $projectId @extra --url $url 2>&1 | Out-String)
  return @{ code = $LASTEXITCODE; out = $out }
}

Write-Host "`n--- normal read ---"
$r = ReadCgw @('src/hello.txt', '--side', 'host')
Expect ($r.code -eq 0 -and $r.out -match '第二行中文') 'reads out the text content' $r.out
$r = ReadCgw @('src/sub/note.md', '--side', 'host')
Expect ($r.code -eq 0 -and $r.out -match '# 标题') 'files in a subdir are readable too' $r.out

# Content is verbatim, so it can be redirected to a local file (use cmd's real redirect; the PowerShell pipeline eats newlines)
cmd /c "`"$cgw`" cat $projectId src/hello.txt --side host --url $url > `"$root\copy.txt`" 2>nul"
$fromCgw = (Get-Content "$root\copy.txt" -Raw)
$fromDisk = (Get-Content "$hostRoot\src\hello.txt" -Raw)
$same = $fromCgw -eq $fromDisk
Expect $same 'redirected local file content matches' "cgw=[$fromCgw] disk=[$fromDisk]"

Write-Host "`n--- truncation ---"
$json = (& $cgw cat $projectId src/big.txt --side host --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
Expect ($json.truncated -eq $true) 'a large file is marked truncated' "$($json.truncated)"
Expect ($json.bytes -eq 262144 -and $json.size -eq 400000) 'only 256 KB read, but the real size is reported' "$($json.bytes)/$($json.size)"
$r = ReadCgw @('src/big.txt', '--side', 'host')
Expect ($r.out -match '只显示了前') 'the CLI hints that it was truncated' $r.out

Write-Host "`n--- binary ---"
$json = (& $cgw cat $projectId src/image.png --side host --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
Expect ($json.binary -eq $true) 'the PNG is judged binary' "$($json.binary)"
Expect ([string]::IsNullOrEmpty($json.text)) 'binary returns no content (text is empty in the JSON)' $json.text
$r = ReadCgw @('src/image.png', '--side', 'host')
Expect ($r.code -eq 0 -and $r.out -match '二进制文件') 'the CLI speaks plainly' $r.out

Write-Host "`n--- boundaries ---"
$r = ReadCgw @('src', '--side', 'host')
Expect ($r.code -ne 0 -and $r.out -match '不是普通文件') 'a directory is rejected' $r.out
$r = ReadCgw @('src/nope.txt', '--side', 'host')
Expect ($r.code -ne 0 -and $r.out -match '读不到|不存在') 'a missing file errors' $r.out
$r = ReadCgw @('../../config.json', '--side', 'host')
Expect ($r.code -ne 0 -and $r.out -match '不允许') '`..` cannot escape' $r.out
$r = ReadCgw @('', '--side', 'host')
Expect ($r.code -eq 1) 'no path given is a usage error' "exit=$($r.code) $($r.out)"
if ($linkMade) {
  $r = ReadCgw @('src/escape.txt', '--side', 'host')
  Expect ($r.code -ne 0 -and $r.out -match '根目录之外|逃出了根目录') 'a symlink escape is rejected' $r.out
} else {
  Write-Host "  (skipping the symlink case: cannot create symlinks on this machine)"
}

Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
if ($bad -gt 0) { Write-Host "`n$bad assertions failed" -ForegroundColor Red; exit 1 }
Write-Host "`nall passed" -ForegroundColor Green
