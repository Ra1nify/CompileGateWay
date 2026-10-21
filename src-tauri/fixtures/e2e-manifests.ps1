# End-to-end fixture for the build-type catalog (manifest/) (design doc §6.2 / §6.4).
#
# This round dropped the "built-in presets" idea: a build type json is just a few
# plain files in `manifest/` next to the exe. This fixture checks that semantic:
#
#   1. **first start**: dir missing -> create it + copy the 4 shipped templates in;
#   2. **second start**: dir exists -> not a single byte touched (edits/deletes stay);
#   3. **renamed from old `variety/`**: moved as-is, nothing copied, lost or overwritten;
#   4. **refresh event**: `cgw manifests refresh` / `POST /api/manifests/reload` /
#      MCP `cgw_reload_manifests` -- re-read from disk, report added / changed / removed;
#   5. **fill templates**: write back only missing ones, never touch the edited copy.
#
# Fully offline, no external machine needed.
#
# Usage: cd src-tauri; pwsh -NoProfile -File fixtures\e2e-manifests.ps1

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'cgw-manifests'
$rel = 'F:\CODEX\CompileGateWay\src-tauri\target\release'

Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $root, "$root\home" | Out-Null
Copy-Item "$rel\compilegateway-daemon.exe" $root
Copy-Item "$rel\compilegateway-cli.exe" $root
Set-Content "$root\config.json" '{ "port": 0, "language": "zh-Hans", "listen": "lan", "logRetainDays": 7 }' -Encoding utf8

$env:USERPROFILE = "$root\home"
$bad = 0
function Expect($cond, $what, $detail) { if ($cond) { Write-Host "  ok   $what" } else { Write-Host "  FAIL $what  $detail"; $script:bad++ } }
function StartDaemon {
  $p = Start-Process -FilePath "$root\compilegateway-daemon.exe" -WorkingDirectory $root -PassThru -WindowStyle Hidden
  for ($i = 0; $i -lt 60; $i++) {
    Start-Sleep -Milliseconds 250
    if (Test-Path "$root\runtime.json") {
      try { $script:url = (Get-Content "$root\runtime.json" -Raw | ConvertFrom-Json).url } catch { }
      if ($script:url) { return $p }
    }
  }
  throw 'daemon did not start'
}
function StopDaemon($p) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue; Start-Sleep -Milliseconds 400 }
function Api([string]$path, [string]$method = 'Get', [string]$body = $null) {
  $a = @{ Uri = "$script:url$path"; Method = $method; SkipHttpErrorCheck = $true; TimeoutSec = 20 }
  if ($body) { $a.ContentType = 'application/json'; $a.Body = $body }
  return Invoke-WebRequest @a
}

$cgw = "$root\compilegateway-cli.exe"

Write-Host "`n--- first start: dir missing -> create it + copy templates ---"
$d = StartDaemon
Expect (Test-Path "$root\manifest") 'the manifest/ dir was created' ''
$files = @(Get-ChildItem "$root\manifest" -Filter *.json -Name)
Expect ($files.Count -eq 4) "4 templates were copied in (actual $($files.Count): $($files -join ', '))" ''
Expect ($files -contains 'kotlin-multiplatform-ios.json') 'the KMP iOS one is there' "$($files -join ', ')"
$onDisk = Get-Content "$root\config.json" -Raw | ConvertFrom-Json
Expect ($null -eq $onDisk.mcp) 'config.json does not need an mcp section (default is on, loopback only)' "$($onDisk | ConvertTo-Json -Compress)"
$list = (Api '/api/manifests').Content | ConvertFrom-Json
Expect ($list.manifests.Count -eq 4) "the API lists 4 too (actual $($list.manifests.Count))" ''
Expect ($list.dir -eq "$root\manifest") "the dir reported by the API is manifest/ ($($list.dir))" ''
$one = $list.manifests | Where-Object { $_.id -eq 'xcode' }
Expect ($one.template -eq $true -and $one.source -eq 'shipped') 'shipped ones are marked template/shipped (only means "can be restored if deleted", not special treatment)' "$($one | ConvertTo-Json -Compress)"
StopDaemon $d

Write-Host "`n--- second start: dir exists -> not a single byte touched ---"
'{ "displayName": "我改过的", "build": { "steps": ["echo hi"] } }' | Set-Content "$root\manifest\xcode.json" -Encoding utf8
Remove-Item "$root\manifest\gradle-android.json"
$d = StartDaemon
Expect ((Get-Content "$root\manifest\xcode.json" -Raw) -match '我改过的') 'your edited file was not overwritten' ''
Expect (-not (Test-Path "$root\manifest\gradle-android.json")) 'your deleted file was not quietly restored' ''
StopDaemon $d

Write-Host "`n--- refresh event: edit / add / delete while the daemon runs, all reported ---"
# This is the real usage: the daemon keeps running, you edit things under
# `manifest/` in your editor (or an AI writes the files), then hit refresh.
# Entering this section the dir holds 3 files (xcode edited, gradle-android deleted above).
$d = StartDaemon
'{ "port": 0, "language": "zh-Hans", "listen": "lan" }' | Set-Content "$root\manifest\brand-new.json" -Encoding utf8
'{ "displayName": "又改了一次", "build": { "steps": ["echo again"] } }' | Set-Content "$root\manifest\xcode.json" -Encoding utf8
Remove-Item "$root\manifest\flutter-ios.json"
$r = (Api '/api/manifests/reload' 'Post' '{}').Content | ConvertFrom-Json
Expect ($r.added -contains 'brand-new') 'the added one is reported' "$($r.added -join ',')"
Expect ($r.changed -contains 'xcode') 'the changed one is reported' "$($r.changed -join ',')"
Expect ($r.removed -contains 'flutter-ios') 'the removed one is reported' "$($r.removed -join ',')"
Expect ($r.total -eq 3) "refresh also brings the list back (total=$($r.total))" ''
Expect ($r.invalid -eq 1) 'the invalid one is named (brand-new lacks displayName)' "invalid=$($r.invalid)"
$again = (Api '/api/manifests/reload' 'Post' '{}').Content | ConvertFrom-Json
Expect ($again.summary -eq '没有变化') 'refreshing again right after says "no changes"' "$($again.summary)"
$cli = (& $cgw manifests refresh --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -eq 0 -and $cli -match '构建类型已刷新') 'cgw manifests refresh works too' $cli
# The log must show it as well (the CLI window / log page shows who refreshed when)
$log = (Api '/api/logs/tail?limit=60').Content | ConvertFrom-Json
Expect (@($log.entries | Where-Object { $_.text -match '构建类型已刷新' }).Count -ge 1) 'a refresh writes a log entry' ''

Write-Host "`n--- whole dir deleted: refresh regenerates it ---"
Remove-Item "$root\manifest" -Recurse -Force
$r = (Api '/api/manifests/reload' 'Post' '{}').Content | ConvertFrom-Json
Expect ($r.created -eq $true -and @($r.seeded).Count -eq 4) 'when the dir is gone, refresh recreates it and seeds templates' "created=$($r.created) seeded=$(@($r.seeded).Count)"
Expect ($r.total -eq 4) "after regenerating there are 4 (total=$($r.total))" ''
StopDaemon $d

Write-Host "`n--- fill templates: write back only missing ones, leave edited ones alone ---"
'{ "displayName": "我又改了一次", "build": { "steps": ["echo mine"] } }' | Set-Content "$root\manifest\xcode.json" -Encoding utf8
Remove-Item "$root\manifest\gradle-android.json"
Remove-Item "$root\manifest\flutter-ios.json"
$d = StartDaemon
$missing = (Api '/api/manifests/templates').Content | ConvertFrom-Json
Expect (@($missing.missing).Count -eq 2) "2 are missing (xcode edited but present, gradle-android and flutter-ios gone)" "$($missing.missing | ConvertTo-Json -Compress)"
$reset = (Api '/api/manifests/reset' 'Post' '{"ids":[]}').Content | ConvertFrom-Json
Expect (@($reset.restored).Count -eq 2) "2 were restored (actual $(@($reset.restored).Count))" "$($reset | ConvertTo-Json -Compress)"
Expect ((Get-Content "$root\manifest\xcode.json" -Raw) -match '我又改了一次') 'restoring does not touch your edited copy' ''
StopDaemon $d

Write-Host "`n--- the old variety/ dir is renamed over as-is ---"
Remove-Item "$root\manifest" -Recurse -Force
New-Item -ItemType Directory -Force -Path "$root\variety" | Out-Null
'{ "displayName": "老用户的改动", "build": { "steps": ["echo legacy"] } }' | Set-Content "$root\variety\xcode.json" -Encoding utf8
'{ "displayName": "老用户自己加的", "build": { "steps": ["echo mine"] } }' | Set-Content "$root\variety\mine.json" -Encoding utf8
$d = StartDaemon
Expect (-not (Test-Path "$root\variety")) 'variety/ was moved away' ''
Expect ((Get-Content "$root\manifest\xcode.json" -Raw) -match '老用户的改动') 'moved content is kept as-is' ''
Expect (Test-Path "$root\manifest\mine.json") 'the self-added copy is there too' ''
$ids = @((Api '/api/manifests').Content | ConvertFrom-Json).manifests.id
Expect ($ids -contains 'mine') 'the API can see it' "$($ids -join ', ')"

# ---------------------------------------------------------------- named build steps
# Design doc §7.6.9 point 3: `build.steps` may be written as `{id, run}`.
# The name is for diagnostics -- a failed build says which step in the log, and that is what we check here.
Write-Host "`n--- named build steps ---"
$named = '{ "displayName": "带名字的步骤", "build": { "steps": [
    { "id": "pods", "run": "echo pods" },
    { "id": "link", "run": "echo link" } ] } }'
$named | Set-Content "$root\named.json" -Encoding utf8
$save = & $cgw manifests save named --file "$root\named.json" --json --url $url 2>&1 | Out-String
Expect ($LASTEXITCODE -eq 0) 'named steps can be saved (old form still works)' "exit=$LASTEXITCODE $save"

# Duplicate step ids must be rejected (it only affects diagnostic quality, but is hard to debug)
$dup = '{ "displayName": "重复", "build": { "steps": [ {"id":"a","run":"echo 1"}, {"id":"a","run":"echo 2"} ] } }'
$dup | Set-Content "$root\dup.json" -Encoding utf8
$rejected = & $cgw manifests save dup --file "$root\dup.json" --url $url 2>&1 | Out-String
Expect ($LASTEXITCODE -ne 0 -and $rejected -match '步骤 id 重复') 'duplicate step ids are rejected' "exit=$LASTEXITCODE $rejected"

# Run one job with it: the log should list the step names first (failing to reach the
# target is expected, but "which steps run next" is logged before connecting -- that is the point of names)
Write-Host "`n--- named build steps: the log says which step ---"
$source = "$root\home\src"
New-Item -ItemType Directory -Force -Path $source | Out-Null
Set-Content "$source\a.txt" 'x' -Encoding utf8
$client = @'
{ "id": "c-named", "name": "offline", "host": "127.0.0.1", "port": 1,
  "auth": { "method": "password", "username": "x", "password": "y" } }
'@
$made = Api '/api/clients' 'Post' $client
Expect ($made.StatusCode -eq 200) 'created an unreachable client' "$($made.StatusCode)"
# The client id is **server-generated** (like jobs), so read it back from the list
$clientId = ((Api '/api/clients').Content | ConvertFrom-Json).clients[-1].id
$project = @{ clientId = $clientId; sourcePath = $source; name = 'named' } | ConvertTo-Json -Compress
$madeProject = Api '/api/projects' 'Post' $project
$projectId = ($madeProject.Content | ConvertFrom-Json).project.id
Expect ([bool]$projectId) 'created a throwaway project to run this step' "$($madeProject.Content)"

$task = '{ "kind": "build", "name": "带名字的构建", "manifest": "named" }'
$saved = & $cgw tasks save $projectId --body $task --json --url $url 2>&1 | Out-String
Expect ($LASTEXITCODE -eq 0) 'created a build task for this project' "exit=$LASTEXITCODE $saved"
$taskId = ((& $cgw tasks $projectId --json --url $url 2>&1 | Out-String) | ConvertFrom-Json |
  Where-Object { $_.manifest -eq 'named' }).id
$run = ((& $cgw run $projectId $taskId --json --url $url 2>&1 | Out-String) | ConvertFrom-Json).jobId
$logs = ''
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Milliseconds 250
  $logs = (Api "/api/jobs/$run/logs").Content
  if ($logs -match '构建分 2 步') { break }
}
Expect ($logs -match '构建分 2 步:pods\(1\) -> link\(2\)') 'the log lists the named steps' ($logs -replace "`n", ' | ')
StopDaemon $d

Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
if ($bad -gt 0) { Write-Host "`n$bad assertions failed" -ForegroundColor Red; exit 1 }
Write-Host "`nall passed" -ForegroundColor Green
