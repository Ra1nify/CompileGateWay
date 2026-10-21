# Round 8 fixture: confirm fileops files/bytes really reach the job record.
#
# Fully offline: every task is host -> host, so copy/move/delete/export need no SSH.
# The client points at 127.0.0.1:1 (never connects), so no real machine is touched.
#
# Pins "push through task must write back lastSync*" (acceptance record 2026-09-28 §4 item 1):
# only the shortcut `/api/projects/{id}/push` used to write sync state, not task / batch runs.
# Sandbox points USERPROFILE at a temp dir, so host_root() = <sandbox>\home\CompileGateway\<slug>.

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'cgw-cnt'
$rel = 'F:\CODEX\CompileGateWay\src-tauri\target\release'

Remove-Item $root -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $root, "$root\home" | Out-Null
Copy-Item "$rel\compilegateway-daemon.exe" $root
Copy-Item "$rel\compilegateway-cli.exe" $root

Set-Content "$root\config.json" '{ "port": 0, "language": "zh-Hans", "listen": "lan", "logRetainDays": 7 }' -Encoding utf8

Set-Content "$root\clients.json" -Encoding utf8 -Value @'
{
  "clients": [
    {
      "id": "c-offline",
      "name": "offline",
      "host": "127.0.0.1",
      "port": 1,
      "auth": { "method": "password", "username": "x", "password": "y", "keyPath": "", "passphrase": "" },
      "tags": [],
      "baseDir": "~/CompileGateway",
      "baseDirResolved": "",
      "hostKey": ""
    }
  ]
}
'@

function Task($id, $name, $kind, $extra) {
  $base = [ordered]@{ id = $id; kind = $kind; name = $name }
  foreach ($k in $extra.Keys) { $base[$k] = $extra[$k] }
  return $base
}

$hostLoc = @{ side = 'host' }
$tasks = @(
  (Task 't-copy'    'copy-a'    'copy'   @{ from = @{ side='host'; path='src/a.txt' }; to = @{ side='host'; path='dst/a.txt' } }),
  (Task 't-move'    'move-b'    'move'   @{ from = @{ side='host'; path='src/b.txt' }; to = @{ side='host'; path='dst/b.txt' } }),
  (Task 't-copydir' 'copy-dir'  'copy'   @{ from = @{ side='host'; path='src/dir' };   to = @{ side='host'; path='dst/dir' } }),
  (Task 't-del'     'del-b'     'delete' @{ target = @{ side='host'; path='dst/b.txt' } }),
  (Task 't-export'  'export-dir' 'export' @{ sources = @(@{ side='host'; path='src/dir' }); outputName = 'out.zip' }),
  (Task 't-pull'    'pull-src'  'pull'   @{ sources = @(@{ side='host'; path='src/a.txt' }); to = @{ side='host'; path='back' } }),
  (Task 't-b-copy'  'b-copy'    'copy'   @{ from = @{ side='host'; path='src/dir' };   to = @{ side='host'; path='dst3/dir' } }),
  (Task 't-b-del'   'b-del'     'delete' @{ target = @{ side='host'; path='dst3/dir' } }),
  (Task 't-b-export' 'b-export' 'export' @{ sources = @(@{ side='host'; path='src/a.txt' }); outputName = 'b.zip' }),
  # push always fails (source dir is real, but the client points at 127.0.0.1:1, unreachable).
  # The point is "even a **failure** must write back sync state": task/batch push used to skip
  # lastSync*, so after a `push -> build` batch, build still said the project was never pushed.
  (Task 't-push'    'push-src'  'push'   @{}),
  # Input/output contract (design doc §7.6.3 / §7.6.4 / §7.6.7):
  # paths use `{{inputs.x}}` with values in variables; declared outputs must really be produced.
  (Task 't-contr'   'contract-copy' 'copy' @{
      from      = @{ side='host'; path='{{inputs.src}}' }
      to        = @{ side='host'; path='{{inputs.dst}}' }
      overwrite = $true
      variables = @{ src = 'src/a.txt'; dst = 'dst2/a.txt' }
      inputs    = @{ src = @{ type='path'; side='host' }; dst = @{ type='path'; side='host' } }
      outputs   = @{ copied = @{ type='file'; side='host'; path='{{inputs.dst}}' } }
    }),
  # Declares a **required** output that is never produced -> step must fail, code artifactMissing
  # (previously exit code 0 meant success, so a missing artifact still counted as ok).
  (Task 't-miss'    'missing-output' 'copy' @{
      from    = @{ side='host'; path='src/a.txt' }
      to      = @{ side='host'; path='dst4/a.txt' }
      outputs = @{ ghost = @{ type='file'; side='host'; path='dst4/nope.txt' } }
    }),
  # Consumer of a step output reference: the target path comes from an output declared and
  # verified by the earlier step (design doc §7.6.5). The 20 B source is deliberate -- the byte
  # count proves it wrote to the resolved reference path, not some same-sized file by accident.
  (Task 't-ref'     'ref-use'   'copy'   @{
      from      = @{ side='host'; path='{{inputs.src}}' }
      to        = @{ side='host'; path='{{inputs.dst}}' }
      overwrite = $true
      variables = @{ src = 'src/dir/y.txt' }
      inputs    = @{ src = @{ type='path'; side='host' }; dst = @{ type='path'; side='host' } }
    }),
  # Small task for the queue (design doc §7.6.6): overwrite, idempotent, can be re-enqueued.
  (Task 't-queue'   'queue-copy' 'copy'  @{
      from      = @{ side='host'; path='src/dir/x.txt' }
      to        = @{ side='host'; path='dst5/x.txt' }
      overwrite = $true
    })
)

$project = [ordered]@{
  id = 'p-cnt'; name = 'cnt'; clientId = 'c-offline'
  # Source dir is the real one in the sandbox (`$hr\src`), so push gets all the way to SSH
  # before failing instead of being blocked by a "source missing" precheck -- like a real failure.
  sourcePath = "$root\home\CompileGateway\cnt\src"; remotePath = '/tmp/cgw-cnt'
  excludes = @(); addedAt = 0
  lastSyncAt = 0; lastSyncOk = $false; lastSyncMessage = ''
  tasks = $tasks
  batches = @(
    @{ id = 'b-mix'; name = 'mix'; steps = @(
        @{ taskId = 't-b-copy' }, @{ taskId = 't-b-del' }, @{ taskId = 't-b-export' }) },
    # Step id + step output reference (design doc §7.6.5): step 2's target path is exactly the
    # artifact path step 1 **verified**; an unresolvable reference fails right away.
    @{ id = 'b-ref'; name = 'ref'; steps = @(
        @{ taskId = 't-contr'; id = 'produce' },
        @{ taskId = 't-ref'; id = 'use'; variables = @{ dst = '{{steps.produce.outputs.copied}}' } }) },
    # Queue batch that "holds" the project: 100 small copies, long enough to observe queueing.
    @{ id = 'b-long'; name = 'long'; steps = @(1..100 | ForEach-Object { @{ taskId = 't-queue' } }) })
  lastBuildAt = 0; lastBuildOk = $false; lastBuildMessage = ''; lastArtifact = ''
}
Set-Content "$root\projects.json" -Encoding utf8 -Value (@{ projects = @($project) } | ConvertTo-Json -Depth 12)

# Host-side file tree (must live under host_root; confine_local locks it down).
$hr = "$root\home\CompileGateway\cnt"
New-Item -ItemType Directory -Force -Path "$hr\src\dir" | Out-Null
Set-Content "$hr\src\a.txt"   -Value ('a' * 100) -NoNewline
Set-Content "$hr\src\b.txt"   -Value ('b' * 250) -NoNewline
Set-Content "$hr\src\dir\x.txt" -Value ('x' * 10) -NoNewline
Set-Content "$hr\src\dir\y.txt" -Value ('y' * 20) -NoNewline

# Start daemon (USERPROFILE points into the sandbox).
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
Write-Host "daemon: $url (pid $($daemon.Id))"

$cgw = "$root\compilegateway-cli.exe"
function Run($taskId) {
  # --wait --json prints the final job JSON to stdout and progress to stderr, so drop stderr.
  $raw = & $cgw run p-cnt $taskId --wait --json --url $url 2>$null | Out-String
  $job = $null
  try { $job = $raw | ConvertFrom-Json } catch { }
  if (-not $job -or -not $job.id) { Write-Host "!! ${taskId}: no job, output:`n$raw"; return $null }
  $steps = $job.steps | ForEach-Object { "$($_.index):$($_.state)" }
  Write-Host ("{0,-12} {1,-4} files={2,-3} bytes={3,-6} steps=[{4}]  {5}" -f `
    $taskId, $job.state, $job.files, $job.bytes, ($steps -join ' '), ($job.message -replace "`n", ' '))
  $job
}

$results = [ordered]@{}
foreach ($t in @('t-copy', 't-move', 't-copydir', 't-del', 't-export', 't-pull', 't-push', 't-contr', 't-miss')) {
  $results[$t] = Run $t
}

Write-Host "`n--- batch b-mix ---"
$braw = & $cgw batch run p-cnt b-mix --wait --json --url $url 2>$null | Out-String
$bjob = $null
try { $bjob = $braw | ConvertFrom-Json } catch { }
if (-not $bjob -or -not $bjob.id) { Write-Host "!! batch: no job:`n$braw"; $bjob = $null }
else {
  "batch {0} files={1} bytes={2}" -f $bjob.state, $bjob.files, $bjob.bytes
  $bjob.steps | ForEach-Object { "  step $($_.index) $($_.kind) $($_.state) :: $($_.message)" }
}

Write-Host "`n--- batch b-ref (step output reference) ---"
$rraw = & $cgw batch run p-cnt b-ref --wait --json --url $url 2>$null | Out-String
$rjob = $null
try { $rjob = $rraw | ConvertFrom-Json } catch { }
if (-not $rjob -or -not $rjob.id) { Write-Host "!! b-ref: no job:`n$rraw"; $rjob = $null }
else {
  "batch {0} files={1} bytes={2}" -f $rjob.state, $rjob.files, $rjob.bytes
  $rjob.steps | ForEach-Object { "  step $($_.index) $($_.kind) $($_.state) :: $($_.message)" }
}

Write-Host "`n--- disk state ---"
Get-ChildItem $hr -Recurse -File | ForEach-Object { $_.FullName.Replace($hr, '') + "  " + $_.Length }
Write-Host "`n--- assertions ---"
$bad = 0
function Expect($cond, $what) { if ($cond) { "  ok   $what" } else { "  FAIL $what"; $script:bad++ } }

Expect ($results['t-copy'].files -eq 1 -and $results['t-copy'].bytes -eq 100)  'copy: 1 file / 100 B'
Expect ($results['t-move'].files -eq 1 -and $results['t-move'].bytes -eq 250)  'move: 1 file / 250 B'
Expect ($results['t-copydir'].files -eq 2 -and $results['t-copydir'].bytes -eq 30) 'copy dir: 2 files / 30 B'
Expect ($results['t-del'].files -eq 1 -and $results['t-del'].bytes -eq 250)    'delete: 1 file / 250 B'
Expect ($results['t-export'].files -ge 2 -and $results['t-export'].bytes -gt 0) 'export: >=2 files / >0 B'
Expect ($results['t-pull'].files -eq 1 -and $results['t-pull'].bytes -eq 100)  'pull: 1 file / 100 B'
Expect ($results['t-export'].artifacts.Count -ge 1)                            'export recorded the artifact path'
# batch = copy dir (2/30) + delete dir (2/30) + export a.txt (1/100) = 5 files / 160 B
Expect ($bjob.files -eq 5 -and $bjob.bytes -eq 160)                            'batch totals 5 files / 160 B'

# Sync state write-back (regression for acceptance record §4 item 1): task push must record lastSync*
$proj = (Invoke-RestMethod "$url/api/projects").projects | Where-Object { $_.id -eq 'p-cnt' }
Expect ($results['t-push'].state -eq 'failed')                                 'push task cannot reach the target, ends as failed'
Expect (-not [string]::IsNullOrWhiteSpace($proj.lastSyncMessage))              'push task wrote back lastSyncMessage (was empty before)'
Expect ($proj.lastSyncOk -eq $false)                                           'push failure recorded as lastSyncOk=false'
# A failed push must not advance lastSyncAt, else the next build passes silently and fails at step 1
Expect ($proj.lastSyncAt -eq 0)                                                'failed push does not advance lastSyncAt'

# Input/output contract (design doc §7.6.3 / §7.6.7)
Expect ($results['t-contr'].state -eq 'ok')                                    'copy task using {{inputs.x}} ran fine'
Expect ($results['t-contr'].files -eq 1 -and $results['t-contr'].bytes -eq 100) 'expanded path pointed at the real source file'
Expect ($results['t-contr'].steps[0].outputs[0] -eq 'host:dst2/a.txt')         'declared output verified and recorded in step outputs'
Expect ($results['t-contr'].artifacts[0] -eq 'dst2/a.txt')                     'verified artifact landed in artifacts'
Expect ($results['t-contr'].inputs.'t-contr.src' -eq 'src/a.txt')              'job stored the input snapshot (§7.6.7)'
# Required output declared but never produced -> must fail with a structured error code
Expect ($results['t-miss'].state -eq 'failed')                                 'task missing a required output no longer counts as success'
Expect ($results['t-miss'].steps[0].code -eq 'artifactMissing')                'failure carries the artifactMissing code'

# Step output reference (design doc §7.6.5): step 2's target comes from step 1's verified output
Expect ($rjob.state -eq 'ok')                                                  'batch with a step output reference ran fine'
Expect ($rjob.files -eq 2 -and $rjob.bytes -eq 120)                            'two steps moved 100 B + 20 B (step 2 wrote the resolved path)'
Expect ($rjob.stepOutputs.'produce.outputs.copied' -eq 'dst2/a.txt')           'job recorded step outputs (retry resolves references from it)'

# ---------------------------------------------------------------- queue (design doc §7.6.6)
# The **scheduling semantics** of `--enqueue` (stays pending while busy, starts itself when
# free, FIFO) are pinned deterministically by the Rust case
# `server::tests::enqueued_run_waits_for_the_project_then_starts_itself`, which holds the project
# with a hand-registered running job instead of betting on who runs first.
# What is checked here is **interface and orchestration**: the CLI --enqueue call works, and once
# really running, submission order = execution order with no overlap (hammer.mjs project-lock idea).
Write-Host "`n--- queue ---"
$longRaw = & $cgw batch run p-cnt b-long --json --url $url 2>$null | Out-String
$jobLong = ($longRaw | ConvertFrom-Json).jobId
$enq1 = (& $cgw run p-cnt t-queue --enqueue --json --url $url 2>$null | Out-String) | ConvertFrom-Json
$enq2 = (& $cgw run p-cnt t-queue --enqueue --json --url $url 2>$null | Out-String) | ConvertFrom-Json
Expect ([bool]$enq1.queued -and [bool]$enq2.queued) 'enqueue explicitly returns queued=true (not fake immediate execution)'
$projQ = Invoke-RestMethod "$url/api/projects"
Expect ($null -ne $projQ.PSObject.Properties['queued']) '/api/projects carries a queued field (UI shows queueing from it)'

function AwaitJob($id) {
  for ($i = 0; $i -lt 400; $i++) {
    Start-Sleep -Milliseconds 250
    $j = (& $cgw job $id --json --url $url 2>$null | Out-String) | ConvertFrom-Json
    if ($j.state -ne 'running' -and $j.state -ne 'pending') { return $j }
  }
  return $null
}
$jobLongJson = AwaitJob $jobLong
$enq1Json = AwaitJob $enq1.jobId
$enq2Json = AwaitJob $enq2.jobId
Expect ($jobLongJson.state -eq 'ok') 'the long batch holding the project finished on its own'
Expect ($enq1Json.state -eq 'ok' -and $enq2Json.state -eq 'ok') 'both queued items eventually succeeded'
Expect ($enq1Json.startedAt -ge $jobLongJson.finishedAt) 'queued item started only after the long batch ended (no overlap)'
Expect ($enq2Json.startedAt -ge $enq1Json.finishedAt) 'second queued item came after the first (FIFO, no preemption)'
Expect ($enq2Json.startedAt -le $enq1Json.startedAt + 60000) 'the two items did not block each other (queue keeps moving)'

# ---------------------------------------------------------------- host-level mutex (§7.6.6 end)
# Only the **config entry point** is checked here (one of three: cgw set); the lock semantics
# (hostBusy, queued work waits for it, another machine unaffected) are pinned by Rust cases.
Write-Host "`n--- host-level mutex (config) ---"
& $cgw set host-lock on --url $url 2>$null | Out-Null
$cfgOn = Invoke-RestMethod "$url/api/config"
Expect ($cfgOn.hostLock -eq $true) 'cgw set host-lock on persisted and took effect immediately'
& $cgw set host-lock off --url $url 2>$null | Out-Null
$cfgOff = Invoke-RestMethod "$url/api/config"
Expect ($cfgOff.hostLock -eq $false) 'cgw set host-lock off switches it back'

Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
if ($bad -gt 0) { Write-Host "`n$bad assertions failed" -ForegroundColor Red; exit 1 }
Write-Host "`nall passed" -ForegroundColor Green
