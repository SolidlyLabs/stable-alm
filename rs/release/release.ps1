# Build, sign and (with -Publish) upload a release, in one go: the only way a release should be made.
# The version is Cargo.toml's. Steps: the release build (target-dist), the exe copied to
# dist\<version>\ (a folder of its own: dist\StableALM.exe may be the copy running), the installer,
# SHA256SUMS.txt and its signature (sign.ps1), then a check of the signature against the signer
# whitelist and of every file against its sum. -Publish checks all of that again right before
# it creates the GitHub release, and uploads only those four files: nothing unsigned goes up.
#   pwsh rs/release/release.ps1                                  build and sign into rs\dist\<version>
#   pwsh rs/release/release.ps1 -Publish -Notes notes.md         ... and create the GitHub release
#   -Key / -Signer: as sign.ps1; -AllowDirty: build with uncommitted changes (never with -Publish)
param(
  [switch]$Publish,
  [string]$Notes = "",
  [string]$Key = (Join-Path $HOME ".ssh\stable_alm_release"),
  [string]$Signer = "release",
  [switch]$AllowDirty
)
$ErrorActionPreference = "Stop"
$rs = Split-Path -Parent $PSScriptRoot
$allowed = Join-Path $PSScriptRoot "allowed_signers"
$version = (Select-String -Path (Join-Path $rs "Cargo.toml") -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw "no version in Cargo.toml" }
$dist = Join-Path $rs "dist\$version"
$exe = Join-Path $dist "StableALM.exe"
$setup = Join-Path $dist "StableALM-Setup-$version.exe"
$sums = Join-Path $dist "SHA256SUMS.txt"
$assets = @($setup, $exe, $sums, "$sums.sig")

function Run($what, [scriptblock]$cmd) {
  & $cmd
  if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" }
}

# The signature verifies against allowed_signers, the sums list exactly the installer and the exe,
# and each file's hash is the one signed.
function Test-Signed {
  foreach ($f in $assets) { if (-not (Test-Path $f)) { throw "missing $f" } }
  cmd /c "ssh-keygen.exe -Y verify -f `"$allowed`" -I $Signer -n stable-alm-release -s `"$sums.sig`" < `"$sums`""
  if ($LASTEXITCODE -ne 0) { throw "SHA256SUMS.txt.sig does not verify against allowed_signers as $Signer" }
  $want = @{}
  foreach ($line in (Get-Content $sums | Where-Object { $_.Trim() })) {
    $hash, $name = $line -split "  ", 2
    $want[$name] = $hash
  }
  $names = @((Split-Path -Leaf $setup), (Split-Path -Leaf $exe))
  if ($want.Count -ne 2 -or ($names | Where-Object { -not $want.ContainsKey($_) })) {
    throw "SHA256SUMS.txt lists $($want.Keys -join ', '), not $($names -join ', ')"
  }
  foreach ($f in @($setup, $exe)) {
    $h = (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower()
    if ($h -ne $want[(Split-Path -Leaf $f)]) { throw "$(Split-Path -Leaf $f) changed after it was signed" }
  }
  Write-Host "verified: $($names -join ' and ') are signed by '$Signer' (SHA256SUMS.txt.sig)"
}

Push-Location $rs
try {
  $dirty = git status --porcelain -- . ':!dist' ':!ethout.json'
  if ($dirty -and ($Publish -or -not $AllowDirty)) { throw "uncommitted changes in rs\ (a release is built from a commit):`n$($dirty -join "`n")" }
  if ($Publish) {
    if (-not $Notes -or -not (Test-Path $Notes)) { throw "-Publish needs -Notes <file> with the release notes" }
    $tag = "v$version"
    gh release view $tag 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { throw "release $tag already exists: bump the version in Cargo.toml first" }
    $head = git rev-parse HEAD
    if (-not (git branch -r --contains $head)) { throw "commit $head is not pushed: push it before publishing" }
  }

  $env:CARGO_TARGET_DIR = Join-Path $rs "target-dist"
  # source paths in panic messages and debug info name the builder's home folder: mapped to neutral
  # names (a change of RUSTFLAGS rebuilds everything, so target-dist keeps to these)
  $repo = Split-Path -Parent $rs
  $env:RUSTFLAGS = "--remap-path-prefix=$(Join-Path $HOME '.cargo')=cargo --remap-path-prefix=$repo=src --remap-path-prefix=$(Join-Path $HOME '.rustup')=rustup"
  Run "cargo build" { cargo build --release }
  New-Item -ItemType Directory -Force $dist | Out-Null
  Copy-Item (Join-Path $rs "target-dist\release\StableALM.exe") $exe -Force
  $leak = [regex]::Matches([Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($exe)), [regex]::Escape($HOME), "IgnoreCase").Count
  if ($leak) { throw "the exe names the home folder $leak times: a path is not remapped" }
  # stale sums never sit next to fresh files
  Remove-Item $sums, "$sums.sig" -ErrorAction SilentlyContinue

  $iscc = @((Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"), "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $iscc) { throw "Inno Setup 6 (ISCC.exe) not found" }
  Run "the installer" { & $iscc /Q "/O$dist" "/DAppVersion=$version" "/DExePath=$exe" (Join-Path $rs "installer\StableALM.iss") }

  Run "signing" { & pwsh -NoProfile -File (Join-Path $PSScriptRoot "sign.ps1") -Files "$setup,$exe" -Key $Key -Signer $Signer -Out $dist }
  Test-Signed

  if ($Publish) {
    # checked again right before the upload: what goes up is what was verified
    Test-Signed
    Run "gh release create" { gh release create "v$version" @assets --title "Stable ALM $version" --notes-file $Notes --target (git rev-parse HEAD) }
    Write-Host "published v$version"
  } else {
    Write-Host "built and signed $version in $dist (not published; add -Publish -Notes <file> to upload)"
  }
} finally {
  Pop-Location
}
