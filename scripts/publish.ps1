# Initialize ONLY an empty wmqfl861/AK-sending repository from this package.
# Uses the user's existing Git credentials. Never stores or requests a token.
# Not executed during package preparation. No force push and no deployment.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Target = 'https://github.com/wmqfl861/AK-sending.git'
$Root = Split-Path -Parent $PSScriptRoot

function Invoke-GitChecked {
    param([string[]] $GitArgs)
    & git @GitArgs
    if ($LASTEXITCODE -ne 0) { throw "Git failed: $($GitArgs -join ' ')" }
}

Push-Location $Root
try {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) { throw 'Git is not installed.' }
    if (Test-Path (Join-Path $Root '.git')) { throw 'Existing local Git history: integrate manually; this script will not change it.' }
    if (-not (Get-Command python -ErrorAction SilentlyContinue)) { throw 'Python 3.11+ is required for repository validation.' }
    & python scripts/check_repository.py
    if ($LASTEXITCODE -ne 0) { throw 'Repository validation failed.' }
    $Refs = @(& git ls-remote $Target)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the target repository; check account access.' }
    if ($Refs.Count -ne 0) { throw 'Remote is not empty. Read its history and integrate on a new branch; do not overwrite.' }
    $Name = & git config user.name
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($Name)) { throw 'Configure your own Git author name first.' }
    $Email = & git config user.email
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($Email)) { throw 'Configure your own Git author email first.' }
    Invoke-GitChecked -GitArgs @('init', '-b', 'main')
    Invoke-GitChecked -GitArgs @('remote', 'add', 'origin', $Target)
    Invoke-GitChecked -GitArgs @('add', '--all')
    Invoke-GitChecked -GitArgs @('commit', '-m', 'chore: initialize AK-sending direct-only blueprint and domain scaffold')
    Invoke-GitChecked -GitArgs @('push', '--set-upstream', 'origin', 'HEAD:refs/heads/main')
    $Local = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read local commit.' }
    $Remote = & git ls-remote --heads origin refs/heads/main
    if ($LASTEXITCODE -ne 0) { throw 'Push returned, but remote verification failed.' }
    if ([string]::IsNullOrWhiteSpace($Remote) -or ($Remote -split '\s+')[0] -ne $Local) {
        throw 'Remote main does not match the prepared commit; inspect before retrying.'
    }
    Write-Host "Published and verified remote main: $Local"
    Write-Host 'Rust tests and five-platform network acceptance are separate; see the validation report.'
} finally {
    Pop-Location
}
