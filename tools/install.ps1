# Build this checkout's factory binary, then install the skill with it. No Python.
# Usage: tools\install.ps1 <absolute skills root>, for example tools\install.ps1 "$env:USERPROFILE\.agents\skills"
param([Parameter(Mandatory = $true)][string]$Destination)
$ErrorActionPreference = 'Stop'
$checkout = Split-Path -Parent $PSScriptRoot
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
# rustup installs into %USERPROFILE%\.cargo\bin, which a fresh shell can leave off PATH.
$homeCargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not $cargo -and (Test-Path $homeCargo)) { $cargo = $homeCargo }
if (-not $cargo) {
  [Console]::Error.WriteLine('install: cargo not found; install rustup from https://rustup.rs, then run the install again')
  exit 2
}
# cwd is the workspace so rustup reads its rust-toolchain.toml.
Push-Location (Join-Path $checkout 'rust')
try { & $cargo build --profile host --locked } finally { Pop-Location }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $checkout 'rust\target' }
& (Join-Path $target 'host\factory.exe') install --source $checkout --destination $Destination --replace
exit $LASTEXITCODE
