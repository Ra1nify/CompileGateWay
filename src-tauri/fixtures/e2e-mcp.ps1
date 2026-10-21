# End-to-end fixture for the MCP adapter layer (design doc §3, §12 Phase 6).
#
# MCP **runs inside the daemon process** (`src-tauri/src/mcp.rs`) on the daemon's **second port**,
# endpoint fixed at `POST /mcp`. This fixture checks it really works there: (1) available on start
# -- runtime.json carries mcpUrl on a different port; (2) protocol surface -- initialize /
# notification / tools/list / resources / tools/call / edges; (3) source marker -- agent work is
# logged with `mcp` (design doc §10); (4) **both CLI and WebUI can configure it** -- a port change
# takes effect at once, a busy port errors clearly, and a working MCP is never taken down.
#
# Fully offline, no MCP client needs to be installed.
#
# Usage: cd src-tauri; pwsh -NoProfile -File fixtures\e2e-mcp.ps1

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'cgw-mcp'
$rel = 'F:\CODEX\CompileGateWay\src-tauri\target\release'

Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $root, "$root\home" | Out-Null
Copy-Item "$rel\compilegateway-daemon.exe" $root
Copy-Item "$rel\compilegateway-cli.exe" $root
Set-Content "$root\config.json" '{ "port": 0, "language": "zh-Hans", "listen": "lan", "logRetainDays": 7 }' -Encoding utf8

$env:USERPROFILE = "$root\home"
$daemon = Start-Process -FilePath "$root\compilegateway-daemon.exe" -WorkingDirectory $root -PassThru -WindowStyle Hidden
$rt = $null
for ($i = 0; $i -lt 60; $i++) {
  Start-Sleep -Milliseconds 250
  if (Test-Path "$root\runtime.json") {
    try { $rt = Get-Content "$root\runtime.json" -Raw | ConvertFrom-Json } catch { }
    if ($rt.url) { break }
  }
}
if (-not $rt.url) { throw 'daemon did not start' }
$url = $rt.url
$mcp = "$($rt.mcpUrl)/mcp"
$cgw = "$root\compilegateway-cli.exe"
Write-Host "daemon: $url"
Write-Host "mcp:    $mcp"

$bad = 0
function Expect($cond, $what, $detail) { if ($cond) { Write-Host "  ok   $what" } else { Write-Host "  FAIL $what  $detail"; $script:bad++ } }

# One JSON-RPC call. Uses -SkipHttpErrorCheck so 4xx still yields a status code and body.
function Rpc([string]$body, [string]$endpoint = $mcp) {
  $r = Invoke-WebRequest -Uri $endpoint -Method Post -ContentType 'application/json' `
    -Body $body -SkipHttpErrorCheck -TimeoutSec 20
  $parsed = $null
  if ($r.Content) { try { $parsed = $r.Content | ConvertFrom-Json } catch { } }
  return @{ status = [int]$r.StatusCode; body = $parsed; raw = $r.Content }
}

function Api([string]$path, [string]$method = 'Get', [string]$body = $null) {
  $args = @{ Uri = "$url$path"; Method = $method; SkipHttpErrorCheck = $true; TimeoutSec = 20 }
  if ($body) { $args.ContentType = 'application/json'; $args.Body = $body }
  return Invoke-WebRequest @args
}

Write-Host "`n--- available on start (second port) ---"
Expect ($rt.mcpEnabled -eq $true) 'runtime.json says MCP is enabled' "$($rt.mcpEnabled)"
Expect ($rt.mcpPort -gt 0) 'runtime.json recorded the MCP port' "$($rt.mcpPort)"
Expect ($rt.mcpPort -ne $rt.port) "MCP really uses a **different** port (daemon $($rt.port) / mcp $($rt.mcpPort))" "$($rt.mcpUrl)"
# This port is **defined independently**: with nothing in config.json it starts at default 3994 and
# yields only if taken. It has no derived relation to the daemon port (not "main port + 1") -- the
# "change port" section below checks that a hard-coded port takes effect exactly as given.
$cfgMcp = (Get-Content "$root\config.json" -Raw | ConvertFrom-Json).mcp
Expect ($null -eq $cfgMcp) 'an old config with no mcp section uses the default (no manual edit)' "$($cfgMcp | ConvertTo-Json -Compress)"

$health = (Invoke-RestMethod -Uri "$($rt.mcpUrl)/healthz" -TimeoutSec 10)
Expect ($health.ok -eq $true) '/healthz responds' "$($health | ConvertTo-Json -Compress)"
Expect ($health.daemonPort -eq $rt.port) 'healthz reports the daemon port' "$($health.daemonPort)"
# The tool count is not hard-coded in two languages (each addition would touch eight places):
# only "both counts agree + key tools still there" is checked; Rust cases pin the exact number.
Expect ($health.tools -ge 37) "healthz reports the tool count ($($health.tools))" "$($health.tools)"

Write-Host "`n--- protocol surface ---"
$init = Rpc '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}'
Expect ($init.status -eq 200) 'initialize returns 200' "$($init.status)"
Expect ($init.body.result.serverInfo.name -eq 'compilegateway') 'serverInfo name is correct' "$($init.body.result.serverInfo | ConvertTo-Json -Compress)"
Expect ($init.body.result.protocolVersion -eq '2024-11-05') 'protocol version echoes the one the client sent' "$($init.body.result.protocolVersion)"
Expect ($init.body.result.capabilities.tools -ne $null -and $init.body.result.capabilities.resources -ne $null) 'declares tools + resources' ''

$notify = Rpc '{"jsonrpc":"2.0","method":"notifications/initialized"}'
Expect ($notify.status -eq 202 -and [string]::IsNullOrEmpty($notify.raw)) 'notification returns 202 with no body' "$($notify.status) [$($notify.raw)]"

$tools = Rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
Expect ($tools.body.result.tools.Count -eq $health.tools) 'tools/list agrees with healthz' "$($tools.body.result.tools.Count) vs $($health.tools)"
foreach ($name in @('cgw_status', 'cgw_set_config', 'cgw_reload_manifests', 'cgw_manifest_status', 'cgw_export_manifest', 'cgw_save_manifest', 'cgw_run_batch')) {
  Expect (@($tools.body.result.tools | Where-Object { $_.name -eq $name }).Count -eq 1) "tool list contains $name" '' 
}
$saveTask = $tools.body.result.tools | Where-Object { $_.name -eq 'cgw_save_task' }
Expect ($saveTask.inputSchema.properties.task.oneOf.Count -eq 7) 'cgw_save_task has 7 oneOf branches' ''
$withRef = ($tools.body.result.tools | Where-Object { ($_ | ConvertTo-Json -Depth 30) -match '\$ref' }).Count
Expect ($withRef -eq 0) "no dangling `$ref in tool schemas (expanded on embed)" "$withRef"

$res = Rpc '{"jsonrpc":"2.0","id":3,"method":"resources/list"}'
Expect ($res.body.result.resources.Count -eq 8) 'resources/list returns 8 (6 schemas + guide + cli)' "$($res.body.result.resources.Count)"
$read = Rpc '{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"cgw://schema/task"}}'
$schema = $read.body.result.contents[0].text | ConvertFrom-Json
Expect ($schema.'$schema' -eq 'http://json-schema.org/draft-07/schema#') 'resources/read returns the draft-07 schema' "$($schema.'$schema')"

Write-Host "`n--- the three teaching layers (instructions / resources / prompts) ---"
# Layer 1: instructions -- many clients paste it straight into the system prompt
$instr = [string]$init.body.result.instructions
Expect ($instr -match 'cgw://guide' -and $instr -match 'cgw://cli') 'instructions names guide and cli (belt and braces)' ''
Expect ($init.body.result.capabilities.prompts -ne $null) 'capabilities declare prompts' ''

# Layer 2: two prose resources -- read on demand, so client truncation of instructions is fine
$guide = Rpc '{"jsonrpc":"2.0","id":30,"method":"resources/read","params":{"uri":"cgw://guide"}}'
$guideText = [string]$guide.body.result.contents[0].text
Expect ($guideText -match 'Getting a project running for the first time' -and $guideText -match 'What to do when it fails') "cgw://guide is a real playbook page ($($guideText.Length) chars)" ''
Expect ($guide.body.result.contents[0].mimeType -eq 'text/markdown') 'guide mimeType is markdown' ''

$cli = Rpc '{"jsonrpc":"2.0","id":31,"method":"resources/read","params":{"uri":"cgw://cli"}}'
$cliDoc = $cli.body.result.contents[0].text | ConvertFrom-Json
Expect ($cliDoc.commands.Count -ge 60) "cgw://cli carries the full command surface ($($cliDoc.commands.Count) entries)" ''
Expect (($cliDoc.exitCodes.PSObject.Properties | Measure-Object).Count -eq 8) 'cgw://cli carries the exit-code contract' ''
Expect ($cliDoc.whenToUseCli -match 'shell') 'cgw://cli says when to use the CLI instead of MCP' ''
# Same table as `cgw help --json` -- spot-check one command added this round
Expect (@($cliDoc.commands | Where-Object { $_.name -eq 'manifests.refresh' }).Count -eq 1) 'same source as `cgw help --json` (has manifests.refresh)' ''

# Layer 3: prompt templates -- one click drops them into the conversation
$prompts = Rpc '{"jsonrpc":"2.0","id":32,"method":"prompts/list"}'
Expect ($prompts.body.result.prompts.Count -ge 3) "prompts/list returns $($prompts.body.result.prompts.Count) templates" ''
$got = Rpc '{"jsonrpc":"2.0","id":33,"method":"prompts/get","params":{"name":"first-build","arguments":{"clientId":"c-demo"}}}'
$gotText = [string]$got.body.result.messages[0].content.text
Expect ($gotText -match 'push' -and $gotText -match 'batch') 'prompts/get returns the first-build flow text' ''
$missingArg = Rpc '{"jsonrpc":"2.0","id":34,"method":"prompts/get","params":{"name":"diagnose-job","arguments":{}}}'
Expect ($missingArg.body.error.code -eq -32602 -and $missingArg.body.error.message -match 'jobId') 'a missing required argument names the argument' "$($missingArg.body.error | ConvertTo-Json -Compress)"


Write-Host "`n--- tools really hit the daemon ---"
$status = Rpc '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"cgw_status","arguments":{}}}'
Expect ($status.body.result.content[0].text -match 'CompileGateway') 'cgw_status works' "$($status.body.result.content[0].text.Substring(0, [Math]::Min(60, $status.body.result.content[0].text.Length)))"

$client = Rpc '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"cgw_save_client","arguments":{"name":"mcp-e2e","host":"127.0.0.1","port":1,"auth":{"method":"password","username":"x","password":"y"}}}}'
Expect ($client.body.result.isError -ne $true) 'save a client via MCP' "$($client.body.result | ConvertTo-Json -Compress -Depth 6)"
$clientId = ($client.body.result.content[0].text | ConvertFrom-Json).client.id

# Really run a job (copy is host-side, so no SSH). Design doc §10 requires seeing at a glance
# "which work the AI did" -- the `source` field on a job is that marker, and must be mcp.
#
# Note: a copy `from` path is **relative to the project root**, and the host-side "project root" is
# `<user home>\CompileGateway\<project name>`, not the source dir. Do not mix the two up.
# Design doc §6.1: browse / registration / probing share one root whitelist (§7 mismatch is fixed).
# The sandbox source sits under %TEMP% and is not whitelisted -- first confirm **it is rejected**.
New-Item -ItemType Directory -Force -Path "$root\src-mcp" | Out-Null
# Build request bodies with ConvertTo-Json: backslashes in Windows paths need escaping in JSON,
# and a hand-written string easily produces invalid JSON (a 400 instead of a tool error).
$deniedArgs = @{
  jsonrpc = '2.0'; id = 20; method = 'tools/call'
  params = @{ name = 'cgw_create_project'; arguments = @{ clientId = $clientId; sourcePath = "$root\src-mcp"; name = 'nope' } }
} | ConvertTo-Json -Compress -Depth 8
$denied = Rpc $deniedArgs
$deniedText = ($denied.body.result.content | Select-Object -First 1).text
Expect ($denied.body.result.isError -eq $true -and $deniedText -match 'allowedRoots') 'a directory outside the whitelist is rejected via MCP too' "$deniedText"
$allow = Rpc (@{
  jsonrpc = '2.0'; id = 21; method = 'tools/call'
  params = @{ name = 'cgw_set_config'; arguments = @{ allowedRoots = @($root) } }
} | ConvertTo-Json -Compress -Depth 8)
Expect ($allow.body.result.isError -ne $true) 'add the directory to allowedRoots via MCP' "$($allow.body.result | ConvertTo-Json -Compress -Depth 5)"

New-Item -ItemType Directory -Force -Path "$root\home\CompileGateway\mcp-e2e\src" | Out-Null
Set-Content "$root\home\CompileGateway\mcp-e2e\src\a.txt" -Value 'from mcp' -NoNewline

$project = Rpc (@"
{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"cgw_create_project","arguments":{"clientId":"$clientId","sourcePath":"$($root -replace '\\','\\')\\src-mcp","name":"mcp-e2e"}}}
"@)
Expect ($project.body.result.isError -ne $true) 'register a project via MCP' "$($project.body.result | ConvertTo-Json -Compress -Depth 6)"
$projectId = ($project.body.result.content[0].text | ConvertFrom-Json).project.id

$task = Rpc (@"
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"cgw_save_task","arguments":{"projectId":"$projectId","task":{"kind":"copy","from":{"side":"host","path":"src"},"to":{"side":"host","path":"out"}}}}}
"@)
Expect ($task.body.result.isError -ne $true) 'create a copy task via MCP' "$($task.body.result | ConvertTo-Json -Compress -Depth 6)"
$taskId = ($task.body.result.content[0].text | ConvertFrom-Json).project.tasks[-1].id

$run = Rpc (@"
{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"cgw_run_task","arguments":{"projectId":"$projectId","taskId":"$taskId"}}}
"@)
Expect ($run.body.result.isError -ne $true) 'run it via MCP' "$($run.body.result | ConvertTo-Json -Compress -Depth 6)"
$jobId = ($run.body.result.content[0].text | ConvertFrom-Json).jobId

$job = $null
for ($i = 0; $i -lt 40; $i++) {
  Start-Sleep -Milliseconds 250
  $job = (Api "/api/jobs/$jobId").Content | ConvertFrom-Json
  if ($job.state -ne 'running') { break }
}
Expect ($job.state -eq 'ok') "job finished ($($job.state))" "$($job.message)"
Expect ($job.source -eq 'mcp') 'job is marked source=mcp (design doc §10: see at a glance what the AI did)' "$($job.source)"

$jobViaMcp = Rpc (@"
{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"cgw_job","arguments":{"jobId":"$jobId"}}}
"@)
Expect ($jobViaMcp.body.result.content[0].text -match '"source":\s*"mcp"') 'reading it back via MCP carries the same marker' ''

# Queue (design doc §7.6.6): `enqueue: true` goes to /enqueue and queues instead of a 409 when busy.
# The project is empty here, so it starts at once; what is checked is that the parameter is known,
# the route is right, queued=true comes back, and it really runs.
# A separate task with overwrite is created -- reusing the one above would hit "target exists".
$enqTask = Rpc (@"
{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"cgw_save_task","arguments":{"projectId":"$projectId","task":{"kind":"copy","name":"queued-copy","from":{"side":"host","path":"src"},"to":{"side":"host","path":"out2"},"overwrite":true}}}}
"@)
$enqTaskId = ($enqTask.body.result.content[0].text | ConvertFrom-Json).project.tasks[-1].id

$enq = Rpc (@"
{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"cgw_run_task","arguments":{"projectId":"$projectId","taskId":"$enqTaskId","enqueue":true}}}
"@)
Expect ($enq.body.result.isError -ne $true) 'cgw_run_task knows the enqueue parameter' "$($enq.body.result | ConvertTo-Json -Compress -Depth 6)"
$enqJob = ($enq.body.result.content[0].text | ConvertFrom-Json).jobId
Expect ($enq.body.result.content[0].text -match '"queued":\s*true') 'the queue path returns queued=true' ''
$enqDone = $null
for ($i = 0; $i -lt 40; $i++) {
  Start-Sleep -Milliseconds 250
  $enqDone = (Api "/api/jobs/$enqJob").Content | ConvertFrom-Json
  if ($enqDone.state -ne 'running' -and $enqDone.state -ne 'pending') { break }
}
Expect ($enqDone.state -eq 'ok') "the queue path really finished ($($enqDone.state))" "$($enqDone.message)"

Write-Host "`n--- edges ---"
$missing = Rpc '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"cgw_job","arguments":{"jobId":"j-nope"}}}'
Expect ($missing.body.result.isError -eq $true) 'errors fold into isError' ''
Expect ($missing.body.result.structuredContent.error.code -eq 'jobNotFound') 'structured error comes back too' "$($missing.body.result.structuredContent.error.code)"
$unknown = Rpc '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"cgw_nope"}}'
Expect ($unknown.body.result.isError -eq $true) 'unknown tool folds into isError' ''
$noMethod = Rpc '{"jsonrpc":"2.0","id":9,"method":"resources/subscribe"}'
Expect ($noMethod.body.error.code -eq -32601) 'an undeclared method returns -32601' "$($noMethod.body.error.code)"
$badJson = Rpc '{ not json'
Expect ($badJson.status -eq 400) 'bad JSON returns 400' "$($badJson.status)"
$get = Invoke-WebRequest -Uri $mcp -SkipHttpErrorCheck -TimeoutSec 10
Expect ([int]$get.StatusCode -eq 405) 'GET /mcp returns 405 (stateless mode has no SSE stream)' "$($get.StatusCode)"
$wrong = Invoke-WebRequest -Uri "$($rt.mcpUrl)/nope" -Method Post -Body '{}' -SkipHttpErrorCheck -TimeoutSec 10
Expect ([int]$wrong.StatusCode -eq 404) 'unknown path returns 404' "$($wrong.StatusCode)"

Write-Host "`n--- CLI can see / change it (same set in the WebUI) ---"
$show = (& $cgw mcp --url $url 2>&1 | Out-String)
Expect ($show -match 'running\s*:\s*true') 'cgw mcp says it is running' $show
Expect ($show -match 'tools\s*:\s*\d+') 'cgw mcp reports the tool count' $show
$mcpUrl = (& $cgw mcp url --url $url 2>&1 | Out-String).Trim()
Expect ($mcpUrl -eq $mcp) 'cgw mcp url gives the address clients should use' "cgw=[$mcpUrl] expected=[$mcp]"
$cfgShow = (& $cgw config show --url $url 2>&1 | Out-String)
Expect ($cfgShow -match [regex]::Escape($rt.mcpUrl)) 'cgw config show also carries MCP' $cfgShow

# Pick a currently free port to switch to (avoid clashing with anything else on this machine)
$probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$probe.Start(); $newPort = $probe.LocalEndpoint.Port; $probe.Stop()

Write-Host "`n--- change port: takes effect at once, no restart ---"
$changed = (& $cgw mcp port $newPort --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -eq 0) "cgw mcp port $newPort succeeded" "$LASTEXITCODE $changed"
$moved = Invoke-RestMethod -Uri "http://127.0.0.1:$newPort/healthz" -TimeoutSec 10
Expect ($moved.ok -eq $true) 'the new port works immediately' "$($moved | ConvertTo-Json -Compress)"
# A hard-coded port must take effect **as given**, not "some nearby number"
$nowPort = ((Api '/api/config').Content | ConvertFrom-Json).mcp.port
Expect ($nowPort -eq $newPort) "hard-coded port takes effect as given (wanted $newPort, got $nowPort)" "$nowPort"
$oldGone = $false
try { Invoke-RestMethod -Uri "$($rt.mcpUrl)/healthz" -TimeoutSec 3 | Out-Null } catch { $oldGone = $true }
Expect $oldGone 'the old port is closed (not two ports listening at once)' ''
Expect ((Get-Content "$root\config.json" -Raw | ConvertFrom-Json).mcp.port -eq $newPort) 'config.json was persisted' ''

Write-Host "`n--- port busy: clear error, and a running MCP must survive ---"
$conflict = (& $cgw mcp port $rt.port --url $url 2>&1 | Out-String)
Expect ($LASTEXITCODE -ne 0) "clashing with the daemon port fails (exit=$LASTEXITCODE)" $conflict
Expect ($conflict -match '占用') 'reports "port busy", not a vague failure' $conflict
$stillThere = $false
try { $stillThere = (Invoke-RestMethod -Uri "http://127.0.0.1:$newPort/healthz" -TimeoutSec 5).ok } catch { }
Expect ($stillThere -eq $true) 'after the failure MCP still runs on the previous port (rolled back)' ''
Expect ((Get-Content "$root\config.json" -Raw | ConvertFrom-Json).mcp.port -eq $newPort) 'the failed attempt was not written to the config file' ''

Write-Host "`n--- turn off / on ---"
& $cgw mcp off --url $url | Out-Null
$offGone = $false
try { Invoke-RestMethod -Uri "http://127.0.0.1:$newPort/healthz" -TimeoutSec 3 | Out-Null } catch { $offGone = $true }
Expect $offGone 'the port is closed after mcp off' ''
& $cgw mcp url --url $url 2>&1 | Out-Null
Expect ($LASTEXITCODE -ne 0) 'after off, cgw mcp url fails clearly (not an empty line)' "$LASTEXITCODE"
& $cgw mcp on --url $url | Out-Null
$backUp = $false
try { $backUp = (Invoke-RestMethod -Uri "http://127.0.0.1:$newPort/healthz" -TimeoutSec 5).ok } catch { }
Expect ($backUp -eq $true) 'after mcp on it is back (reusing the configured port)' ''
$statusJson = (Api '/api/status').Content | ConvertFrom-Json
Expect ($statusJson.mcp.running -eq $true -and $statusJson.mcp.endpoint -match '/mcp$') '/api/status also carries the MCP endpoint (the status page reads it)' "$($statusJson.mcp.endpoint)"

Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
if ($bad -gt 0) { Write-Host "`n$bad assertions failed" -ForegroundColor Red; exit 1 }
Write-Host "`nall passed" -ForegroundColor Green
