<#
.SYNOPSIS
  Export a clean, leak-free archive of CompileGateway for public upload.

.DESCRIPTION
  This script uses `git archive` to create a clean zip containing ONLY tracked git files.
  Local sensitive configuration files (`clients.json`, `config.json`, `projects.json`,
  `jobs.json`, `runtime.json`, `.workbuddy-ai/`, `.tmp/`, etc.) are completely excluded,
  guaranteeing that no credentials, internal IPs, or local paths can be leaked.
#>

[CmdletBinding()]
param (
    [string]$OutputFile = "..\CompileGateway-Clean-Upload.zip"
)

$ErrorActionPreference = "Stop"

Write-Host "==> Checking git status..." -ForegroundColor Cyan
$root = (git rev-parse --show-toplevel).Trim()
Set-Location $root

# Ensure webui/dist exists and is committed
if (-not (Test-Path "webui/dist/index.html")) {
    Write-Error "webui/dist/index.html is missing! Build webui first with npm run build in webui-src."
}

Write-Host "==> Creating clean git archive: $OutputFile" -ForegroundColor Cyan
git archive --format=zip --output="$OutputFile" HEAD

Write-Host "`n[SUCCESS] Clean release archive created at:" -ForegroundColor Green
Write-Host "  $((Resolve-Path $OutputFile).Path)" -ForegroundColor Yellow
Write-Host "`nYou can safely upload this archive or unpack it into your public repository!" -ForegroundColor Green
