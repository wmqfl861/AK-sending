# AK-sending Windows packaging script (local packaging only; never uploads).
# Compatible with Windows PowerShell 5.1. No network upload, no git operations.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -Profile debug
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -SkipFlutter:$false
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -OutDir D:\tmp\ak-sending
#
# Parameters:
#   -OutDir       Staging directory (wiped and recreated if it already exists).
#                 Default: <repo>\target\dist\ak-sending-<version>-windows-x64, kept under
#                 target\ so the existing /target/ .gitignore rule covers all artifacts.
#   -Profile      Cargo profile: 'release' (default) or 'debug'.
#   -SkipFlutter  Defaults to true. When false, the script probes
#                 flutter_app\build\windows\x64\runner\Release\ for *.exe and data\;
#                 both must exist to be packaged. A missing Flutter build is NOT an
#                 error (expected until the UI wave lands).
#                 NOTE: powershell.exe -File passes argument values as literal strings
#                 ([switch]/[bool] parameters reject them), so this parameter is typed
#                 [object] and normalized at runtime. Accepted values: $true/$false,
#                 true/false, 1/0, yes/no, on/off (any case).
#
# Output:
#   ak-cli.exe, ak_ffi.dll, README.md (+ LICENSE only if the repo ever gains one),
#   flutter\ (optional), MANIFEST.txt (SHA256 + size per file), plus a .zip next
#   to OutDir.

param(
    [string]$OutDir,
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release',
    [object]$SkipFlutter = $true
)

$ErrorActionPreference = 'Stop'

function Fail([string]$Message) {
    [Console]::Error.WriteLine("error: $Message")
    exit 1
}

function ConvertTo-BooleanFlag([object]$Value, [string]$Name) {
    if ($Value -is [bool]) { return $Value }
    if ($Value -is [int] -or $Value -is [long] -or $Value -is [double] -or $Value -is [decimal]) {
        return ([double]$Value) -ne 0
    }
    if ($Value -is [string]) {
        $text = $Value.Trim().ToLowerInvariant()
        if ($text -in @('true', '$true', '1', 'yes', 'y', 'on')) { return $true }
        if ($text -in @('false', '$false', '0', 'no', 'n', 'off')) { return $false }
    }
    Fail ("cannot interpret -{0} value '{1}' (use $true/$false, true/false, 1/0, yes/no)" -f $Name, $Value)
}

# powershell.exe -File hands argument values over as literal strings, so the
# flexible boolean flags are normalized here rather than via parameter types.
$SkipFlutter = ConvertTo-BooleanFlag -Value $SkipFlutter -Name 'SkipFlutter'

$RepoRoot = Split-Path -Parent $PSScriptRoot
Write-Host "==> repo root: $RepoRoot"

try {
    # --- 1. read workspace version from the root Cargo.toml [workspace.package] ---
    $Version = $null
    $section = ''
    foreach ($line in Get-Content -LiteralPath (Join-Path $RepoRoot 'Cargo.toml')) {
        if ($line -match '^\s*\[(.+?)\]\s*$') {
            $section = $Matches[1]
        }
        elseif ($section -eq 'workspace.package' -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            $Version = $Matches[1]
            break
        }
    }
    if (-not $Version) { Fail 'cannot read [workspace.package] version from Cargo.toml' }
    Write-Host "==> workspace version: $Version"

    if (-not $OutDir) {
        $OutDir = Join-Path (Join-Path $RepoRoot 'target\dist') ("ak-sending-" + $Version + "-windows-x64")
    }
    $OutDirFull = [System.IO.Path]::GetFullPath($OutDir)
    if ($OutDirFull.TrimEnd('\') -eq $RepoRoot.TrimEnd('\')) {
        Fail '-OutDir must not be the repository root (it gets wiped)'
    }

    # --- 2. cargo build (any failure => non-zero exit) ---
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail 'cargo not found in PATH (needs ~/.cargo/bin on PATH)'
    }
    $buildArgs = @('build', '-p', 'ak-cli', '-p', 'ak-ffi')
    if ($Profile -eq 'release') { $buildArgs += '--release' }
    Write-Host "==> cargo $($buildArgs -join ' ')"
    Push-Location $RepoRoot
    try {
        & cargo @buildArgs
        $cargoExit = $LASTEXITCODE
    }
    finally {
        Pop-Location
    }
    if ($cargoExit -ne 0) { Fail "cargo build failed (exit code $cargoExit)" }

    # --- 3. stage artifacts into OutDir (wipe + recreate for idempotence) ---
    $profileDir = Join-Path $RepoRoot ("target\" + $Profile)
    $cliExe = Join-Path $profileDir 'ak-cli.exe'
    $ffiDll = Join-Path $profileDir 'ak_ffi.dll'
    if (-not (Test-Path -LiteralPath $cliExe -PathType Leaf)) { Fail "expected cargo artifact missing: $cliExe" }
    if (-not (Test-Path -LiteralPath $ffiDll -PathType Leaf)) { Fail "expected cargo artifact missing: $ffiDll" }

    if (Test-Path -LiteralPath $OutDirFull) {
        Write-Host "==> clearing existing OutDir: $OutDirFull"
        Remove-Item -LiteralPath $OutDirFull -Recurse -Force
    }
    New-Item -ItemType Directory -Path $OutDirFull -Force | Out-Null

    Copy-Item -LiteralPath $cliExe -Destination $OutDirFull
    Copy-Item -LiteralPath $ffiDll -Destination $OutDirFull
    Write-Host "==> included: ak-cli.exe, ak_ffi.dll (from target\$Profile)"

    $readme = Join-Path $RepoRoot 'README.md'
    if (-not (Test-Path -LiteralPath $readme -PathType Leaf)) { Fail 'README.md not found at repository root' }
    Copy-Item -LiteralPath $readme -Destination $OutDirFull
    Write-Host '==> included: README.md'

    # License: the repository currently has none. Nothing is fabricated; if the
    # owner adds a license file later it gets picked up automatically.
    $licenseNames = @('LICENSE', 'LICENSE.txt', 'LICENSE.md', 'LICENSE-MIT', 'LICENSE-APACHE',
                      'LICENCE', 'LICENCE.txt', 'LICENCE.md', 'COPYING', 'COPYING.txt')
    $licensePath = $null
    foreach ($name in $licenseNames) {
        $candidate = Join-Path $RepoRoot $name
        if (Test-Path -LiteralPath $candidate -PathType Leaf) { $licensePath = $candidate; break }
    }
    if ($licensePath) {
        Copy-Item -LiteralPath $licensePath -Destination $OutDirFull
        Write-Host "==> included: $(Split-Path -Leaf $licensePath)"
    }
    else {
        Write-Host '==> no license file (owner decision pending)'
    }

    # --- 4. optional Flutter app ---
    if ($SkipFlutter) {
        Write-Host '==> flutter app skipped (-SkipFlutter defaults to true)'
    }
    else {
        $flutterRelease = Join-Path $RepoRoot 'flutter_app\build\windows\x64\runner\Release'
        $flutterExes = @()
        $haveData = $false
        if (Test-Path -LiteralPath $flutterRelease -PathType Container) {
            $flutterExes = @(Get-ChildItem -LiteralPath $flutterRelease -Filter '*.exe' -File -ErrorAction SilentlyContinue)
            $haveData = Test-Path -LiteralPath (Join-Path $flutterRelease 'data') -PathType Container
        }
        if ($flutterExes.Count -gt 0 -and $haveData) {
            $flutterOut = Join-Path $OutDirFull 'flutter'
            New-Item -ItemType Directory -Path $flutterOut -Force | Out-Null
            foreach ($exe in $flutterExes) { Copy-Item -LiteralPath $exe.FullName -Destination $flutterOut }
            Get-ChildItem -LiteralPath $flutterRelease -Filter '*.dll' -File -ErrorAction SilentlyContinue |
                ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $flutterOut }
            Copy-Item -LiteralPath (Join-Path $flutterRelease 'data') -Destination $flutterOut -Recurse
            Write-Host ("==> flutter app included: {0} exe(s) + dlls + data\ (from {1})" -f $flutterExes.Count, $flutterRelease)
        }
        else {
            Write-Host 'flutter app not built yet; skipping (expected until the UI wave)'
        }
    }

    # --- 5. MANIFEST.txt (SHA256 via Get-FileHash + size, one line per file) ---
    $staged = @(Get-ChildItem -LiteralPath $OutDirFull -Recurse -File | Sort-Object FullName)
    $manifest = New-Object System.Collections.Generic.List[string]
    $manifest.Add('AK-sending Windows package manifest')
    $manifest.Add("version : $Version")
    $manifest.Add("profile : $Profile")
    $manifest.Add("created : $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') local time; local packaging only, not uploaded")
    $manifest.Add('format  : SHA256  bytes  relative-path (SHA256 via PowerShell Get-FileHash)')
    $manifest.Add('')
    foreach ($file in $staged) {
        $rel = ($file.FullName.Substring($OutDirFull.Length + 1)) -replace '\\', '/'
        $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $manifest.Add(('{0} {1,12} {2}' -f $hash, $file.Length, $rel))
    }
    $totalBytes = ($staged | Measure-Object -Property Length -Sum).Sum
    $manifest.Add('')
    $manifest.Add("$($staged.Count) file(s), $totalBytes byte(s) total (excluding this MANIFEST.txt)")
    [System.IO.File]::WriteAllLines((Join-Path $OutDirFull 'MANIFEST.txt'), $manifest)
    Write-Host "==> wrote MANIFEST.txt ($($staged.Count) file(s) hashed)"

    # --- 6. zip (sibling of OutDir, outside the zipped tree) ---
    $zipPath = $OutDirFull + '.zip'
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    Compress-Archive -Path $OutDirFull -DestinationPath $zipPath
    $zipItem = Get-Item -LiteralPath $zipPath

    # --- 7. summary ---
    $finalFiles = @(Get-ChildItem -LiteralPath $OutDirFull -Recurse -File)
    $finalBytes = ($finalFiles | Measure-Object -Property Length -Sum).Sum
    Write-Host ''
    Write-Host '==> package summary'
    Write-Host ("    outdir : {0}" -f $OutDirFull)
    Write-Host ("    files  : {0} ({1} bytes total)" -f $finalFiles.Count, $finalBytes)
    Write-Host ("    zip    : {0} ({1} bytes)" -f $zipPath, $zipItem.Length)
    Write-Host '    upload : none (local packaging only; see docs/PUBLISHING.zh-CN.md)'
    exit 0
}
catch {
    Fail ($_.Exception.Message)
}
