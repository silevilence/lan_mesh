$ErrorActionPreference = "Stop"

$AppDir = Split-Path -Parent $PSScriptRoot
$DistDir = Join-Path $AppDir "dist"
$DistSrcDir = Join-Path $DistDir "src"

if (Test-Path -LiteralPath $DistDir) {
    $ResolvedDist = (Resolve-Path -LiteralPath $DistDir).ProviderPath
    if ($ResolvedDist -ne (Join-Path (Resolve-Path -LiteralPath $AppDir).ProviderPath "dist")) {
        throw "Unexpected frontend distribution path: $ResolvedDist"
    }
    Remove-Item -LiteralPath $ResolvedDist -Recurse -ErrorAction Stop
}
New-Item -ItemType Directory -Path $DistSrcDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $AppDir "index.html") -Destination (Join-Path $DistDir "index.html") -Force
foreach ($Asset in "main.js", "ui-model.mjs", "group-forms.mjs", "dom.mjs", "chat-sidebar.mjs", "chat-timeline.mjs", "feedback.mjs", "update-ui.mjs", "app.css") {
    Copy-Item -LiteralPath (Join-Path $AppDir "src\$Asset") -Destination (Join-Path $DistSrcDir $Asset) -Force
}
Copy-Item -LiteralPath (Join-Path $AppDir "src-tauri\icons\128x128.png") -Destination (Join-Path $DistDir "icon.png") -Force
