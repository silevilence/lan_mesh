$ErrorActionPreference = "Stop"

$AppDir = Split-Path -Parent $PSScriptRoot
$old = @{
  RUSTFLAGS = $env:RUSTFLAGS
  CARGO_PROFILE_RELEASE_CODEGEN_UNITS = $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS
  CARGO_PROFILE_RELEASE_LTO = $env:CARGO_PROFILE_RELEASE_LTO
  CARGO_PROFILE_RELEASE_OPT_LEVEL = $env:CARGO_PROFILE_RELEASE_OPT_LEVEL
  CARGO_PROFILE_RELEASE_PANIC = $env:CARGO_PROFILE_RELEASE_PANIC
  CARGO_PROFILE_RELEASE_STRIP = $env:CARGO_PROFILE_RELEASE_STRIP
}

try {
  Set-Location $AppDir
  & (Join-Path $PSScriptRoot "prepare-dist.ps1")

  $env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1"
  $env:CARGO_PROFILE_RELEASE_LTO = "true"
  $env:CARGO_PROFILE_RELEASE_OPT_LEVEL = "z"
  $env:CARGO_PROFILE_RELEASE_PANIC = "abort"
  $env:CARGO_PROFILE_RELEASE_STRIP = "symbols"

  $hostLine = & rustc -vV | Select-String -Pattern "^host:"
  if ($hostLine -and $hostLine.ToString().Contains("msvc")) {
    $env:RUSTFLAGS = (($env:RUSTFLAGS, "-C link-arg=/OPT:REF -C link-arg=/OPT:ICF") | Where-Object { $_ }) -join " "
  }

  cargo tauri build --bundles nsis
  if ($LASTEXITCODE -ne 0) {
    throw "Tauri NSIS build failed with exit code $LASTEXITCODE."
  }

  $workspaceRoot = Split-Path -Parent $AppDir
  $releaseCandidates = @(
    (Join-Path $workspaceRoot "target\release\lan-mesh-tauri.exe"),
    (Join-Path $AppDir "src-tauri\target\release\lan-mesh-tauri.exe")
  )
  $releaseExecutable = $releaseCandidates |
    Where-Object { Test-Path -LiteralPath $_ } |
    ForEach-Object { Get-Item -LiteralPath $_ } |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1
  if (-not $releaseExecutable) {
    throw "Release executable was not produced."
  }

  & (Join-Path $PSScriptRoot "verify-windows-icon.ps1") `
    -ExecutablePath $releaseExecutable.FullName
} finally {
  foreach ($key in $old.Keys) {
    if ($null -eq $old[$key]) {
      Remove-Item -LiteralPath "Env:$key" -ErrorAction SilentlyContinue
    } else {
      Set-Item -LiteralPath "Env:$key" -Value $old[$key]
    }
  }
}
