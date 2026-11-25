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
# The stamp factory install checks before it trusts this build, computed as install.sh does: a hash of every
# file under rust\ but rust\target and under skills\factory, then the binary's sha256. The inputs are hashed
# before the build, so an edit made during it does not match.
$files = @(Get-ChildItem -LiteralPath (Join-Path $checkout 'rust') -Force | Where-Object Name -ne 'target' |
    ForEach-Object { if ($_.PSIsContainer) { Get-ChildItem -LiteralPath $_.FullName -Recurse -File -Force } else { $_ } }) +
  @(Get-ChildItem -LiteralPath (Join-Path $checkout 'skills\factory') -Recurse -File -Force)
[string[]]$rels = $files | Where-Object { -not ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) } |
  ForEach-Object { $_.FullName.Substring($checkout.Length + 1).Replace('\', '/') }
[Array]::Sort($rels, [StringComparer]::Ordinal)
$lines = foreach ($rel in $rels) {
  (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $checkout $rel)).Hash.ToLowerInvariant() + '  ' + $rel + "`n"
}
$digest = [Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::UTF8.GetBytes(-join $lines))
$inputs = -join ($digest | ForEach-Object { $_.ToString('x2') })
# cwd is the workspace so rustup reads its rust-toolchain.toml. The build stays in rust\target, where
# factory install recognizes its own fresh build and caches it instead of compiling the crate again.
Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
Push-Location (Join-Path $checkout 'rust')
try { & $cargo build --profile host --locked } finally { Pop-Location }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$exe = Join-Path $checkout 'rust\target\host\factory.exe'
$binary = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$exe.stamp", "$inputs`n$binary`n")
& $exe install --source $checkout --destination $Destination --replace
exit $LASTEXITCODE
