# End-to-end fixture for known_hosts import / export.
#
# The value here is **offline pre-trust**: pin the host key before the first
# connect -- `ssh-keyscan host | cgw clients hostkey <id> --import -`.
#
# Fully offline: only the "store / read / reject" logic is checked, no SSH at all.
# The client host is a nonexistent domain and the port is deliberately not 22
# (which also exercises the `[host]:port` rule).
#
# Usage: cd src-tauri; pwsh -NoProfile -File fixtures\e2e-known-hosts.ps1

$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'cgw-kh'
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

# These are the **test public keys** from the rust tests (no private keys), so the fixture is reproducible.
$KEY_A = 'AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF'
$KEY_B = 'AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ'
$KEY_C = 'AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X'
# Hostname hash from a fixed salt (the example hash was computed independently with .NET HMACSHA1)
$SALT = 'AAECAwQFBgcICQ=='
$HASH_2222 = 'MzK8soy4EFk6wWfNnOh8us+JjKQ='

function AddClient([string]$name, [string]$h, [int]$p) {
  & $cgw clients add --host $h --port $p --user u --password p --name $name --url $url | Out-Null
  $all = (& $cgw clients list --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
  return ($all.clients | Where-Object { $_.name -eq $name }).id
}

function HostKey([string]$id, [string[]]$extra) {
  $out = (& $cgw clients hostkey $id @extra --url $url 2>&1 | Out-String)
  return @{ code = $LASTEXITCODE; out = $out.Trim() }
}

$target = AddClient 'kh-target' 'example.com' 2222
$other = AddClient 'kh-other' 'other.example.com' 22
$empty = AddClient 'kh-empty' 'nothing.example.com' 22
Write-Host "clients: target=$target other=$other empty=$empty"

# A decent known_hosts: a comment, another machine, and the target's plain + hashed records (both lines point at one key)
$file = "$root\known_hosts"
@"
# comment line
other.example.com ssh-ed25519 $KEY_C
[example.com]:2222 ssh-ed25519 $KEY_A
|1|$SALT|$HASH_2222 ssh-ed25519 $KEY_A
this line is garbage
"@ | Set-Content $file -Encoding utf8

# ---------------------------------------------------------------- import
Write-Host "`n--- import ---"
$r = HostKey $target @('--import', $file)
Expect ($r.code -eq 0) 'imported from the file' $r.out
Expect ($r.out -match '已从第 3 行导入') 'reported the matched line number' $r.out

$all = (& $cgw clients list --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
$entry = $all.clients | Where-Object { $_.id -eq $target }
Expect ($entry.hostKey -like 'SHA256:*') 'fingerprint recorded' $entry.hostKey
Expect ($entry.hasKeyLine -eq $true) 'public key body recorded' "$($entry.hasKeyLine)"

# Import again: same key, so it should report "already this one"
$r = HostKey $target @('--import', $file)
Expect ($r.code -eq 0 -and $r.out -match '本来就是这把钥匙') 'a repeated import neither errors nor changes anything' $r.out

# Another machine in the same file (port 22, plain record) must import too
$r = HostKey $other @('--import', $file)
Expect ($r.code -eq 0) 'another machine in the same file imports too (plain port-22 record)' $r.out

# ---------------------------------------------------------------- export
Write-Host "`n--- export ---"
$r = HostKey $target @('--export')
Expect ($r.code -eq 0) 'export succeeded' $r.out
$expected = "[example.com]:2222 ssh-ed25519 $KEY_A"
Expect ($r.out -eq $expected) 'export is a standard known_hosts line (with port)' "got: $($r.out)"

$r = HostKey $other @('--export')
Expect ($r.code -eq 0 -and $r.out -eq "other.example.com ssh-ed25519 $KEY_C") 'the other machine exports correctly too' $r.out

$r = HostKey $empty @('--export')
Expect ($r.code -eq 3 -and $r.out -match '还没有记录任何主机钥匙') 'export before ever connecting is rejected explicitly' $r.out

# Export -> import round trip: feed the line exported from target into a new client with the same host
$clone = AddClient 'kh-clone' 'example.com' 2222
$r = HostKey $target @('--export')
$r.out | Set-Content "$root\exported" -Encoding utf8
$r = HostKey $clone @('--import', "$root\exported")
Expect ($r.code -eq 0) 'the exported line imports back (round trip matches)' $r.out
$all = (& $cgw clients list --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
$cloneEntry = $all.clients | Where-Object { $_.id -eq $clone }
$targetEntry = $all.clients | Where-Object { $_.id -eq $target }
Expect ($cloneEntry.hostKey -eq $targetEntry.hostKey) 'fingerprints match after the round trip' "$($cloneEntry.hostKey) vs $($targetEntry.hostKey)"

# Reading from stdin is supported (the ssh-keyscan pipeline)
$r = HostKey $empty @('--import', '-')
Expect ($r.code -eq 1 -and $r.out -match '没有内容') 'empty stdin is reported as empty (exit code 1)' $r.out
Get-Content $file | & $cgw clients hostkey $empty --import - --url $url | Out-Null
$all = (& $cgw clients list --json --url $url 2>&1 | Out-String) | ConvertFrom-Json
Expect (($all.clients | Where-Object { $_.id -eq $empty }).hasKeyLine -eq $false) 'stdin has no nothing.example.com, so nothing was written' ''

# ---------------------------------------------------------------- rejection cases
Write-Host "`n--- what must be rejected is rejected ---"
# Note: this client uses port 2222, so the file must write `[example.com]:2222`
@"
[example.com]:2222 ssh-ed25519 $KEY_B
"@ | Set-Content "$root\different" -Encoding utf8
$r = HostKey $target @('--import', "$root\different")
Expect ($r.code -eq 3 -and $r.out -match '已经有一把不同的钥匙') 'replacing an existing record needs an explicit --force' $r.out
$r = HostKey $target @('--import', "$root\different", '--force')
Expect ($r.code -eq 0) 'with --force it can be replaced' $r.out
# Put it back so later cases line up
HostKey $target @('--import', $file, '--force') | Out-Null

@"
@revoked [example.com]:2222 ssh-ed25519 $KEY_A
"@ | Set-Content "$root\revoked" -Encoding utf8
$r = HostKey $target @('--import', "$root\revoked", '--force')
Expect ($r.code -eq 3 -and $r.out -match '@revoked') 'a @revoked key is never trusted' $r.out

@"
[example.com]:2222 ssh-ed25519 $KEY_A
[example.com]:2222 ssh-ed25519 $KEY_B
"@ | Set-Content "$root\ambiguous" -Encoding utf8
$r = HostKey $target @('--import', "$root\ambiguous", '--force')
Expect ($r.code -eq 3 -and $r.out -match '不猜') 'two keys for one machine are not guessed, candidates are listed' $r.out

@"
example.com ssh-ed25519 $KEY_A
"@ | Set-Content "$root\plain-only" -Encoding utf8
$r = HostKey $target @('--import', "$root\plain-only", '--force')
Expect ($r.code -eq 3 -and $r.out -match '\[example\.com\]:2222') 'a port mismatch explains itself (write [host]:port)' $r.out

@"
@cert-authority [example.com]:2222 ssh-ed25519 $KEY_A
"@ | Set-Content "$root\ca" -Encoding utf8
$r = HostKey $target @('--import', "$root\ca", '--force')
Expect ($r.code -eq 3 -and $r.out -match 'cert-authority') 'a CA record is not treated as a host key' $r.out

$r = HostKey $target @('--import', "$root\does-not-exist")
Expect ($r.code -eq 1 -and $r.out -match '失败') 'a missing file is a usage error (exit code 1)' $r.out

# A fingerprint-only record cannot export the standard form, but must give a readable fallback and a hint
& $cgw clients hostkey $target --forget --url $url | Out-Null
& $cgw clients hostkey $target --trust 'SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA' --url $url | Out-Null
$r = HostKey $target @('--export')
Expect ($r.code -eq 0 -and $r.out -match '^#') 'with only a fingerprint it exports as a comment' $r.out
$r = HostKey $target @('--export', '--json')
Expect ($r.out -match 'hasKeyLine' -and $r.out -match 'warning') '--json says clearly what is missing' $r.out
$r = HostKey $target @()
Expect ($r.out -match '只有指纹') 'viewing the fingerprint hints that there is no public key body' $r.out

Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
if ($bad -gt 0) { Write-Host "`n$bad assertions failed" -ForegroundColor Red; exit 1 }
Write-Host "`nall passed" -ForegroundColor Green
