param(
  [Parameter(Mandatory = $true)]
  [string]$ExecutablePath,
  [string]$IconPath,
  [double]$MaximumMeanDifference = 8
)

$ErrorActionPreference = "Stop"

if ([string]::IsNullOrWhiteSpace($IconPath)) {
  $IconPath = Join-Path $PSScriptRoot "..\src-tauri\icons\icon.ico"
}

$resolvedExecutable = (Resolve-Path -LiteralPath $ExecutablePath).Path
$resolvedIcon = (Resolve-Path -LiteralPath $IconPath).Path

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class NativeIconExtractor {
  [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
  public static extern uint ExtractIconEx(
    string file,
    int iconIndex,
    IntPtr[] largeIcons,
    IntPtr[] smallIcons,
    uint iconCount
  );

  [DllImport("user32.dll", SetLastError = true)]
  [return: MarshalAs(UnmanagedType.Bool)]
  public static extern bool DestroyIcon(IntPtr icon);
}
"@

function Extract-Icon([string]$Path) {
  $large = [IntPtr[]]::new(1)
  $small = [IntPtr[]]::new(1)
  $count = [NativeIconExtractor]::ExtractIconEx($Path, 0, $large, $small, 1)
  if ($count -eq 0) {
    throw "No Windows icon is embedded in $Path"
  }

  $handle = if ($small[0] -ne [IntPtr]::Zero) { $small[0] } else { $large[0] }
  $icon = [System.Drawing.Icon]::FromHandle($handle).Clone()
  if ($small[0] -ne [IntPtr]::Zero) {
    [void][NativeIconExtractor]::DestroyIcon($small[0])
  }
  if ($large[0] -ne [IntPtr]::Zero -and $large[0] -ne $small[0]) {
    [void][NativeIconExtractor]::DestroyIcon($large[0])
  }
  return $icon
}

$embeddedIcon = Extract-Icon $resolvedExecutable
$expectedIcon = Extract-Icon $resolvedIcon
$embeddedBitmap = New-Object System.Drawing.Bitmap(32, 32)
$expectedBitmap = New-Object System.Drawing.Bitmap(32, 32)
$embeddedGraphics = [System.Drawing.Graphics]::FromImage($embeddedBitmap)
$expectedGraphics = [System.Drawing.Graphics]::FromImage($expectedBitmap)
$embeddedGraphics.Clear([System.Drawing.Color]::White)
$expectedGraphics.Clear([System.Drawing.Color]::White)
$embeddedGraphics.DrawIcon($embeddedIcon, 0, 0)
$expectedGraphics.DrawIcon($expectedIcon, 0, 0)
$embeddedGraphics.Dispose()
$expectedGraphics.Dispose()

try {
  $difference = 0L
  for ($y = 0; $y -lt 32; $y++) {
    for ($x = 0; $x -lt 32; $x++) {
      $actual = $embeddedBitmap.GetPixel($x, $y)
      $expected = $expectedBitmap.GetPixel($x, $y)
      $difference += [Math]::Abs([int]$actual.R - [int]$expected.R)
      $difference += [Math]::Abs([int]$actual.G - [int]$expected.G)
      $difference += [Math]::Abs([int]$actual.B - [int]$expected.B)
    }
  }

  $meanDifference = $difference / (32 * 32 * 3)
  Write-Host "Embedded icon mean RGB difference: $([Math]::Round($meanDifference, 2))"
  if ($meanDifference -gt $MaximumMeanDifference) {
    throw "Windows executable embeds a stale icon: $resolvedExecutable"
  }
} finally {
  $expectedBitmap.Dispose()
  $embeddedBitmap.Dispose()
  $expectedIcon.Dispose()
  $embeddedIcon.Dispose()
}

Write-Host "Windows executable icon matches $resolvedIcon"
