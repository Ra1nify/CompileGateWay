# End-to-end fixture for export archive + download (design doc §9).
#
# §9 says the Export button "only offers packaged archive download", so this
# chain is verified down to the **byte**:
#   export (merge into one export job) -> list -> download -> unzip and compare.
#
# Fully offline: host side (source=host), no SSH needed.
#
# Usage: cd src-tauri; pwsh -NoProfile -File fixtures\e2e-export.ps1

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'cgw-export'
$rel = 'F:\CODEX\CompileGateWay\src-tauri\target\release'

Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $root, "$root\home", "$root\down" | Out-Null
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

# ---------------------------------------------------------------- project + content
& $cgw clients add --host 127.0.0.1 --port 1 --user x --password y --name offline --url $url | Out-Null
$clients = (& $cgw clients list --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
# Design doc §6.1: browse / register project / detect build type **share one root allowlist**.
# Acceptance note §7 caught exactly the "browse says no, register says yes" case -- now both agree:
# the temp dir is not in the allowlist, so registering **is rejected** and the error says what to do.
New-Item -ItemType Directory -Force -Path "$root\src" | Out-Null
$denied = (& $cgw projects add --client $clients.clients[0].id --path "$root\src" --name 被拒的 --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -ne 0 -and $denied -match 'allowedRoots') 'a dir outside the allowlist is rejected (same check as browse)' $denied
& $cgw config set --allowed-roots "$root" --url $url | Out-Null
& $cgw projects add --client $clients.clients[0].id --path "$root\src" --name 导出演示 --url $url | Out-Null
$projects = (& $cgw projects --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
$projectId = $projects.projects[0].id

# Host-managed dir: host_root = <USERPROFILE>\CompileGateway\<slug(project name)>
$hostRoot = "$root\home\CompileGateway\导出演示"
New-Item -ItemType Directory -Force -Path "$hostRoot\src\sub" | Out-Null
Set-Content "$hostRoot\src\a.txt" -Value ('a' * 300) -NoNewline
Set-Content "$hostRoot\src\sub\b.txt" -Value ('b' * 120) -NoNewline
Set-Content "$hostRoot\src\中文名.txt" -Value '中文内容测试' -NoNewline
# Symlink: zip cannot express it (classic ZIP needs Unix extra attributes, many unzippers ignore them),
# so export skips it -- but it **must say so in the message**, never silently drop one
$linkMade = $false
try {
  New-Item -ItemType SymbolicLink -Path "$hostRoot\src\link.txt" -Target 'a.txt' -ErrorAction Stop | Out-Null
  $linkMade = $true
} catch { }
Write-Host "project $projectId, content in place: $hostRoot\src"

# ---------------------------------------------------------------- export
Write-Host "`n--- export ---"
$out = (& $cgw export $projectId --source host --path src --name demo.zip --wait --json --url $url 2>$null | Out-String)
$job = $null
try { $job = $out | ConvertFrom-Json } catch { }
Expect ($job -and $job.state -eq 'ok') 'export job ran ok' $out
# 300 + 120 + the CJK content file (6 CJK chars = 18 UTF-8 bytes)
Expect ($job -and $job.files -eq 3 -and $job.bytes -eq 438) 'recorded 3 files / 438 B' "$($job.files)/$($job.bytes)"
if ($linkMade) {
  Expect ($job.message -match '跳过了 1 个符号链接') 'a skipped symlink is reported' $job.message
}

$zip = "$hostRoot\exports\demo.zip"
Expect (Test-Path $zip) "zip lands in <host-managed dir>\exports\ dir" $zip

# Default name (design doc §9: `{project name}-{unix timestamp}.zip`)
$out = (& $cgw export $projectId --source host --path src --wait --json --url $url 2>$null | Out-String)
$defaultName = (($out | ConvertFrom-Json).artifacts | Select-Object -First 1)
$leaf = Split-Path $defaultName -Leaf
Expect ($leaf -match '^导出演示-\d+\.zip$') 'default filename is `{project name}-{unix sec}.zip`' $leaf

# No path given and no build artifacts -> reject explicitly, not silently pack the whole project
$out = (& $cgw export $projectId --source host --wait --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -eq 3 -and $out -match '还没有构建产物') 'no path given is rejected explicitly (exit code 3)' $out

# ---------------------------------------------------------------- list
Write-Host "`n--- list ---"
$list = (& $cgw exports $projectId --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
Expect ($list.exports.Count -eq 2) 'lists two archives' "actual $($list.exports.Count)"
$demo = $list.exports | Where-Object { $_.name -eq 'demo.zip' }
Expect ($demo -and $demo.bytes -eq (Get-Item $zip).Length) 'listed size matches disk' "$($demo.bytes)"

# ---------------------------------------------------------------- download
Write-Host "`n--- download ---"
$out = (& $cgw exports $projectId demo.zip --save "$root\down" --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -eq 0 -and (Test-Path "$root\down\demo.zip")) 'downloads into the given dir' $out
$downloaded = Get-Item "$root\down\demo.zip"
Expect ($downloaded.Length -eq (Get-Item $zip).Length) 'downloaded byte count matches the source file' "$($downloaded.Length) vs $((Get-Item $zip).Length)"

# Binary header: must be zip (PK\x03\x04)
$head = [System.IO.File]::ReadAllBytes("$root\down\demo.zip")[0..3]
Expect ($head[0] -eq 0x50 -and $head[1] -eq 0x4b -and $head[2] -eq 0x03 -and $head[3] -eq 0x04) 'downloaded bytes are a real zip (magic PK\003\004)' ($head -join ',')

# Unzip and compare content -- size alone cannot prove "the bytes are right"
# The top level inside the archive is the **source name** (`src`), because export stages the source into a same-named dir before packing
Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead("$root\down\demo.zip")
$entries = $archive.Entries | ForEach-Object { $_.FullName } | Sort-Object
$archive.Dispose()
# Directories are written as entries too (so empty dirs are not lost), so the list has both kinds
$expectedEntries = 'src/', 'src/a.txt', 'src/sub/', 'src/sub/b.txt', 'src/中文名.txt'
Expect (($entries -join '|') -eq (($expectedEntries | Sort-Object) -join '|')) 'archive entry structure is correct (top level is the source name)' ($entries -join '|')

$unzip = "$root\unzip"
Remove-Item $unzip -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive -Path "$root\down\demo.zip" -DestinationPath $unzip -Force
$same = (Get-FileHash "$unzip\src\a.txt" -Algorithm SHA256).Hash -eq (Get-FileHash "$hostRoot\src\a.txt" -Algorithm SHA256).Hash
Expect $same 'unzipped content is byte-identical (a.txt)' ''
Expect ((Get-Content "$unzip\src\中文名.txt" -Raw) -eq '中文内容测试') 'Chinese file name and content both intact' ''

# An explicit file name must download too (--save given a file name)
$out = (& $cgw exports $projectId demo.zip --save "$root\down\renamed.bin" --url $url 2>&1 | Out-String)
Expect ((Test-Path "$root\down\renamed.bin") -and (Get-Item "$root\down\renamed.bin").Length -eq $downloaded.Length) '--save with a file name writes to that name' $out

# ---------------------------------------------------------------- escape / edge
Write-Host "`n--- escape and edge ---"
$out = (& $cgw exports $projectId '..%2F..%2Fconfig.json' --save "$root\down\stolen" --url $url 2>&1 | Out-String)
$stolen = Test-Path "$root\down\stolen"
Expect (-not $stolen -or (Get-Item "$root\down\stolen").Length -eq 0) 'path traversal cannot fetch files outside exports' $out

$out = (& $cgw exports $projectId 'nope.zip' --save "$root\down\nope" --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -eq 6 -and $out -match 'nope.zip') 'an unknown name returns 404 (exit code 6)' $out

Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
if ($bad -gt 0) { Write-Host "`n$bad assertions failed" -ForegroundColor Red; exit 1 }
Write-Host "`nall passed" -ForegroundColor Green
