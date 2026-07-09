$ErrorActionPreference = "Stop"

$AppDir = Split-Path -Parent $PSScriptRoot
$DistDir = Join-Path $AppDir "dist"
$DistSrcDir = Join-Path $DistDir "src"

# ponytail: static frontend, add a JS bundler only when this grows beyond two files.
Remove-Item -LiteralPath $DistDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $DistSrcDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $AppDir "index.html") -Destination (Join-Path $DistDir "index.html") -Force
Copy-Item -LiteralPath (Join-Path $AppDir "src\main.js") -Destination (Join-Path $DistSrcDir "main.js") -Force
