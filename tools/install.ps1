# Build this checkout's factory binary, then install the skill with it. No Python.
# Usage: tools\install.ps1 <absolute skills root>, for example tools\install.ps1 "$env:USERPROFILE\.agents\skills"
param([string]$Destination)
$ErrorActionPreference = 'Stop'
if (-not $Destination) {
  [Console]::Error.WriteLine('usage: tools\install.ps1 <absolute skills root>')
  exit 2
}
$checkout = Split-Path -Parent $PSScriptRoot
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
# rustup installs into %USERPROFILE%\.cargo\bin, which a fresh shell can leave off PATH.
$homeCargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not $cargo -and (Test-Path $homeCargo)) { $cargo = $homeCargo }
if (-not $cargo) {
  [Console]::Error.WriteLine('install: cargo not found; install rustup with rustup-init.exe from https://win.rustup.rs/x86_64 (see https://rustup.rs), then run the install again')
  exit 2
}
# cwd is the workspace so rustup reads its rust-toolchain.toml. The build stays in rust\target, where
# factory install recognizes its own fresh build and caches it instead of compiling the crate again.
Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
Push-Location (Join-Path $checkout 'rust')
try { & $cargo build --profile host --locked } finally { Pop-Location }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& (Join-Path $checkout 'rust\target\host\factory.exe') install --source $checkout --destination $Destination --replace
exit $LASTEXITCODE
