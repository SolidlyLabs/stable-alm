# Sign a release: writes SHA256SUMS.txt for the given files and SHA256SUMS.txt.sig, an SSH signature
# of it in namespace "stable-alm-release", then checks it against the signer whitelist the app has
# built in (rs/release/allowed_signers). Upload both files with the release; the app's updater
# offers a release only when this signature verifies (src/update.rs).
#   pwsh rs/release/sign.ps1 -Files rs\dist\StableALM-Setup-0.7.0.exe, rs\dist\StableALM.exe
#   -Key: the signing key (default ~/.ssh/stable_alm_release); -Signer: its name in allowed_signers
$ErrorActionPreference = "Stop"
# arguments by hand: -Files a, b arrives in several shapes (pwsh -File, & from a shell)
$Files = @(); $Key = Join-Path $HOME ".ssh\stable_alm_release"; $Signer = "release"; $Out = ""; $opt = "-Files"
foreach ($a in $args) {
  if ($a -is [string] -and $a -match '^-(Files|Key|Signer|Out)$') { $opt = $a; continue }
  switch ($opt) {
    "-Files" { $Files += @($a) | ForEach-Object { "$_" -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ } }
    "-Key" { $Key = "$a"; $opt = "-Files" }
    "-Signer" { $Signer = "$a"; $opt = "-Files" }
    "-Out" { $Out = "$a"; $opt = "-Files" }
  }
}
if (-not $Files) { throw "give the files to sign: -Files a.exe, b.exe" }
$allowed = Join-Path $PSScriptRoot "allowed_signers"
if (-not $Out) { $Out = Split-Path -Parent (Resolve-Path $Files[0]) }
$sums = Join-Path $Out "SHA256SUMS.txt"
$lines = foreach ($f in $Files) { "{0}  {1}" -f (Get-FileHash -Algorithm SHA256 $f).Hash.ToLower(), (Split-Path -Leaf $f) }
[IO.File]::WriteAllText($sums, ($lines -join "`n") + "`n")
Remove-Item "$sums.sig" -ErrorAction SilentlyContinue
& ssh-keygen.exe -Y sign -f $Key -n stable-alm-release $sums
if ($LASTEXITCODE -ne 0) { throw "signing failed" }
# a file redirect, so the bytes checked are the bytes signed
cmd /c "ssh-keygen.exe -Y verify -f `"$allowed`" -I $Signer -n stable-alm-release -s `"$sums.sig`" < `"$sums`""
if ($LASTEXITCODE -ne 0) { throw "the signature does not verify against allowed_signers as $Signer" }
Write-Host "signed: $sums and $sums.sig"
