# AK-sending Windows packaging script (local packaging only; never uploads).
# Compatible with Windows PowerShell 5.1. No network upload, no git operations.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -Profile debug
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -SkipFlutter:$false
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package_windows.ps1 -IncludeAndroid
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
#   -IncludeAndroid
#                 Defaults to false. A real [switch]: pass it bare
#                 (-IncludeAndroid) or with a colon (-IncludeAndroid:$true).
#                 When set, the script:
#                   1. runs `bash scripts\build_android_ffi.sh` (Git Bash; builds
#                      libak_ffi.so for x86_64 + aarch64 and stages both into
#                      flutter_app\android\app\src\main\jniLibs\). Existence of the
#                      script AND bash is probed first; a build failure is a WARNING,
#                      never fatal - whatever .so files are then present are still
#                      probed and packaged (so a pre-built tree works offline).
#                   2. copies every jniLibs\<abi>\libak_ffi.so into
#                      <OutDir>\android\jniLibs\<abi>\libak_ffi.so (the layout an
#                      Android app consumes directly);
#                   3. if target\x86_64-linux-android\release\ak-cli exists (an ELF
#                      the ffi build does NOT produce - cross-build ak-cli manually),
#                      copies it as android\ak-cli-android-x86_64.
#                 Everything staged lands in MANIFEST.txt automatically.
#
# Output:
#   ak-cli.exe, ak_ffi.dll, README.md (+ LICENSE only if the repo ever gains one),
#   ak_sending_registry.db (empty SQLite template; a zero-length file is a valid
#   empty SQLite database, the v2 registry server creates its schema on first
#   open), ak_sending_registry.db.README.txt (what the template is / how to
#   back it up), start-registry-server.bat (launcher for `ak-cli
#   registry-server` against the packaged template; extra args pass through),
#   flutter\ (optional), android\ (optional, -IncludeAndroid), MANIFEST.txt
#   (SHA256 + size per file - the registry files above are counted in it),
#   plus a .zip next to OutDir.

param(
    [string]$OutDir,
    [ValidateSet('release', 'debug')]
    [string]$Profile = 'release',
    [object]$SkipFlutter = $true,
    [switch]$IncludeAndroid
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

    # --- 3b. registry-server starter kit (wave 21, B7) ---
    # ak_sending_registry.db: an intentionally EMPTY SQLite template. SQLite
    # treats a zero-length file as a valid empty database (file-format docs:
    # an empty file is an empty DB), and `ak-cli registry-server` runs
    # CREATE TABLE IF NOT EXISTS + PRAGMA user_version when it opens the
    # store (crates/ak-signal/src/server_v2/store.rs), so the template needs
    # no schema baked in and cannot drift from the server's DDL.
    $registryDb = Join-Path $OutDirFull 'ak_sending_registry.db'
    [System.IO.File]::WriteAllBytes($registryDb, (New-Object byte[] 0))

    $registryReadmeLines = @(
        'ak_sending_registry.db - v2 pairing registry SQLite template',
        '=================================================================',
        '',
        'This file is an intentionally EMPTY SQLite database (a zero-length file',
        'is a valid empty SQLite database; the server creates its schema on',
        'first open, so the template carries no schema and cannot drift from',
        'the server DDL).',
        '',
        'Purpose: persistent store of the authoritative v2 pairing registry',
        '(invite codes, pairing transactions, pairing relations, request',
        'journal - see docs/contracts/storage-schema.md in the repository).',
        '',
        'Usage:',
        '  start-registry-server.bat            (default port 7200, TLS-only)',
        '  start-registry-server.bat --port N --bind 127.0.0.1',
        'The launcher runs: ak-cli.exe registry-server --id-dir registry-id',
        '                    --db-path ak_sending_registry.db',
        'On first start the server creates the tables (CREATE TABLE IF NOT',
        'EXISTS, PRAGMA user_version), writes its long-term identity key to',
        'registry-id\registry.key and prints the registry id that devices pin',
        '(--registry-fp). ak-cli.exe must sit next to this file.',
        '',
        'Operations:',
        '  - back up: stop the server, copy ak_sending_registry.db.',
        '  - the database holds invite-code hashes and pairing state - treat it',
        '    as server-private data; do not publish or reuse it across servers.',
        '  - deleting the file resets the registry to empty (all invites and',
        '    pairing relations are lost).')
    [System.IO.File]::WriteAllText((Join-Path $OutDirFull 'ak_sending_registry.db.README.txt'),
        (($registryReadmeLines -join "`r`n") + "`r`n"))

    # start-registry-server.bat: plain cmd launcher, ASCII-only, CRLF lines.
    # Extra arguments (%*) pass through to `ak-cli registry-server`
    # (--port / --bind), so operators can override the listen address
    # without editing the script.
    $batLines = @(
        '@echo off',
        'rem AK-sending v2 pairing registry server launcher.',
        'rem Runs ak-cli.exe registry-server against ak_sending_registry.db;',
        'rem schema and registry key are created on first start.',
        'rem Extra args pass through, e.g.: start-registry-server.bat --port 7200',
        'setlocal',
        'set "HERE=%~dp0"',
        'if not exist "%HERE%ak-cli.exe" (',
        '  echo error: ak-cli.exe not found next to this script - unzip the full package',
        '  exit /b 1',
        ')',
        '"%HERE%ak-cli.exe" registry-server --id-dir "%HERE%registry-id" --db-path "%HERE%ak_sending_registry.db" %*',
        'endlocal')
    [System.IO.File]::WriteAllText((Join-Path $OutDirFull 'start-registry-server.bat'),
        (($batLines -join "`r`n") + "`r`n"))
    Write-Host '==> included: ak_sending_registry.db (empty SQLite template) + README + start-registry-server.bat'

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

            # C09 (reports/codex-crash-analysis-2026-11-07.md): the runner dir
            # can hold a STALE ak_ffi.dll left by an older CMake build, while
            # the root of this package carries the DLL cargo just built in
            # step 2. Two different sources for "the" ak_ffi.dll in one
            # package could mix builds. Unify: the cargo artifact
            # (target\$Profile\ak_ffi.dll) is authoritative - overwrite the
            # flutter\ copy with it, then SHA256-verify BOTH packaged copies
            # against the source so a mismatched package fails loudly.
            $flutterDll = Join-Path $flutterOut 'ak_ffi.dll'
            Copy-Item -LiteralPath $ffiDll -Destination $flutterDll -Force
            $sourceDllHash = (Get-FileHash -LiteralPath $ffiDll -Algorithm SHA256).Hash
            foreach ($dllCopy in @((Join-Path $OutDirFull 'ak_ffi.dll'), $flutterDll)) {
                $copyHash = (Get-FileHash -LiteralPath $dllCopy -Algorithm SHA256).Hash
                if ($copyHash -ne $sourceDllHash) {
                    Fail ("packaged ak_ffi.dll does not match target\{0}\ak_ffi.dll (SHA256 differs): {1}" -f $Profile, $dllCopy)
                }
            }
            Write-Host ("==> flutter\ak_ffi.dll unified with target\{0}\ak_ffi.dll (SHA256 {1}... verified on both copies)" -f $Profile, $sourceDllHash.Substring(0, 16))

            Write-Host ("==> flutter app included: {0} exe(s) + dlls + data\ (from {1})" -f $flutterExes.Count, $flutterRelease)
        }
        else {
            Write-Host 'flutter app not built yet; skipping (expected until the UI wave)'
        }
    }

    # --- 5. optional Android artifacts (-IncludeAndroid) ---
    $androidIncluded = 0
    if (-not $IncludeAndroid) {
        Write-Host '==> android artifacts skipped (-IncludeAndroid not set)'
    }
    else {
        # 5a. refresh the cross-compiled .so via the Git Bash build script.
        #     Existence probes + WARNING on failure: packaging must stay
        #     usable on a box without the NDK (stale-but-present .so files
        #     are still picked up below, and the manifest records them).
        $buildSh = Join-Path $RepoRoot 'scripts\build_android_ffi.sh'
        $bash = $null
        foreach ($candidate in @(
                (Join-Path $env:ProgramFiles 'Git\bin\bash.exe'),
                (Join-Path $env:ProgramFiles 'Git\usr\bin\bash.exe'),
                (Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe'))) {
            if (Test-Path -LiteralPath $candidate -PathType Leaf) { $bash = $candidate; break }
        }
        if (-not $bash) {
            $bashCmd = Get-Command bash -ErrorAction SilentlyContinue
            if ($bashCmd -and $bashCmd.Source -notlike '*\System32\bash.exe') { $bash = $bashCmd.Source }
        }
        $ffiBuilt = $false
        if (-not (Test-Path -LiteralPath $buildSh -PathType Leaf)) {
            Write-Warning "scripts\build_android_ffi.sh not found - skipping the android FFI build (probing staged .so files only)"
        }
        elseif (-not $bash) {
            Write-Warning 'Git Bash (bash.exe) not found - skipping the android FFI build (probing staged .so files only)'
        }
        else {
            Write-Host '==> bash scripts\build_android_ffi.sh (non-fatal on failure)'
            # PS 5.1 wraps redirected native stderr in NativeCommandError
            # records (the first one is displayed, and with
            # $ErrorActionPreference='Stop' it would even abort the script),
            # so the build runs through Start-Process whose stdout/stderr
            # redirection is plain Win32 handle redirection - no PowerShell
            # stream semantics, no error records, output captured to logs.
            $ffiOut = Join-Path $env:TEMP ('ak_android_ffi_out_' + [Guid]::NewGuid().ToString('N') + '.log')
            $ffiErr = Join-Path $env:TEMP ('ak_android_ffi_err_' + [Guid]::NewGuid().ToString('N') + '.log')
            # SDK/NDK discovery lives inside the build script (default
            # $USERPROFILE\android-toolchain\sdk; see docs\ANDROID_BUILD.zh-CN.md).
            $proc = Start-Process -FilePath $bash -ArgumentList 'scripts/build_android_ffi.sh' `
                -WorkingDirectory $RepoRoot -NoNewWindow -Wait -PassThru `
                -RedirectStandardOutput $ffiOut -RedirectStandardError $ffiErr
            foreach ($log in @($ffiOut, $ffiErr)) {
                Get-Content -LiteralPath $log -ErrorAction SilentlyContinue |
                    ForEach-Object { Write-Host "    $_" }
                Remove-Item -LiteralPath $log -Force -ErrorAction SilentlyContinue
            }
            $buildExit = $proc.ExitCode
            if ($buildExit -ne 0) {
                Write-Warning "build_android_ffi.sh exited $buildExit - continuing with whatever .so files are staged (they may be stale)"
            }
            else {
                $ffiBuilt = $true
            }
        }

        # 5b. stage jniLibs\<abi>\libak_ffi.so -> android\jniLibs\<abi>\libak_ffi.so
        $jniLibs = Join-Path $RepoRoot 'flutter_app\android\app\src\main\jniLibs'
        $soFiles = @()
        if (Test-Path -LiteralPath $jniLibs -PathType Container) {
            $soFiles = @(Get-ChildItem -LiteralPath $jniLibs -Filter 'libak_ffi.so' -Recurse -File -ErrorAction SilentlyContinue)
        }
        if ($soFiles.Count -eq 0) {
            Write-Warning "no libak_ffi.so under $jniLibs - the android\ section of the package will be empty of .so files"
        }
        else {
            $androidOut = Join-Path $OutDirFull 'android'
            New-Item -ItemType Directory -Path $androidOut -Force | Out-Null
            foreach ($so in $soFiles) {
                # expected file shape: ...\jniLibs\<abi>\libak_ffi.so
                $abi = Split-Path -Leaf (Split-Path -Parent $so.FullName)
                $abiDir = Join-Path (Join-Path $androidOut 'jniLibs') $abi
                New-Item -ItemType Directory -Path $abiDir -Force | Out-Null
                Copy-Item -LiteralPath $so.FullName -Destination $abiDir
                $androidIncluded++
            }
            Write-Host ("==> android: {0} libak_ffi.so staged into android\jniLibs\ (ffi build {1})" -f `
                    $soFiles.Count, $(if ($ffiBuilt) { 'fresh' } else { 'pre-existing/not rebuilt' }))
        }

        # 5c. the android ak-cli ELF, if someone cross-built it manually
        #     (build_android_ffi.sh only builds ak-ffi, so absence is normal).
        $androidCli = Join-Path $RepoRoot 'target\x86_64-linux-android\release\ak-cli'
        if (Test-Path -LiteralPath $androidCli -PathType Leaf) {
            if (-not (Test-Path -LiteralPath (Join-Path $OutDirFull 'android') -PathType Container)) {
                New-Item -ItemType Directory -Path (Join-Path $OutDirFull 'android') -Force | Out-Null
            }
            Copy-Item -LiteralPath $androidCli -Destination (Join-Path $OutDirFull 'android\ak-cli-android-x86_64')
            $androidIncluded++
            Write-Host '==> android: ak-cli-android-x86_64 staged (from target\x86_64-linux-android\release\ak-cli)'
        }
        else {
            Write-Host '==> android: target\x86_64-linux-android\release\ak-cli absent (expected: the ffi build does not build ak-cli) - not packaged'
        }
        if ($androidIncluded -eq 0) {
            Write-Warning '-IncludeAndroid was set but nothing android was staged (see the warnings above)'
        }
    }

    # --- 6. MANIFEST.txt (SHA256 via Get-FileHash + size, one line per file) ---
    $staged = @(Get-ChildItem -LiteralPath $OutDirFull -Recurse -File | Sort-Object FullName)
    $manifest = New-Object System.Collections.Generic.List[string]
    $manifest.Add('AK-sending Windows package manifest')
    $manifest.Add("version : $Version")
    $manifest.Add("profile : $Profile")
    $manifest.Add('android : ' + $(if ($androidIncluded -gt 0) { "included ($androidIncluded file(s) under android\)" } else { 'not included' }))
    $manifest.Add('registry: ak_sending_registry.db (empty SQLite template) + ak_sending_registry.db.README.txt + start-registry-server.bat')
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

    # --- 7. zip (sibling of OutDir, outside the zipped tree) ---
    $zipPath = $OutDirFull + '.zip'
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    Compress-Archive -Path $OutDirFull -DestinationPath $zipPath
    $zipItem = Get-Item -LiteralPath $zipPath

    # --- 8. summary ---
    $finalFiles = @(Get-ChildItem -LiteralPath $OutDirFull -Recurse -File)
    $finalBytes = ($finalFiles | Measure-Object -Property Length -Sum).Sum
    Write-Host ''
    Write-Host '==> package summary'
    Write-Host ("    outdir : {0}" -f $OutDirFull)
    Write-Host ("    files  : {0} ({1} bytes total)" -f $finalFiles.Count, $finalBytes)
    Write-Host ("    android: {0}" -f $(if ($androidIncluded -gt 0) { "included ($androidIncluded file(s) under android\)" } else { 'not included' }))
    Write-Host ("    zip    : {0} ({1} bytes)" -f $zipPath, $zipItem.Length)
    Write-Host '    upload : none (local packaging only; see docs/PUBLISHING.zh-CN.md)'
    exit 0
}
catch {
    Fail ($_.Exception.Message)
}
