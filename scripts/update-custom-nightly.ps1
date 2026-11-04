$ErrorActionPreference = 'Stop'

function Invoke-Checked {
	param(
		[Parameter(Mandatory = $true)][string]$FilePath,
		[Parameter(Mandatory = $true)][string[]]$Arguments
	)
	& $FilePath @Arguments
	if ($LASTEXITCODE -ne 0) {
		throw "$FilePath $($Arguments -join ' ') failed with exit code $LASTEXITCODE."
	}
}

$repository = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $repository

$branch = (& git branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($branch) -or $branch -eq 'main') {
	throw 'Run this only from your Serein customization branch, never from main.'
}

$worktree = (& git status --porcelain)
if ($LASTEXITCODE -ne 0) {
	throw 'Could not inspect the repository state.'
}
if ($worktree) {
	throw 'Commit or otherwise preserve your source changes before updating; nothing was changed.'
}

$installed = Join-Path $env:LOCALAPPDATA 'Programs\Serein'
if (-not (Test-Path -LiteralPath (Join-Path $installed 'serein.exe')) -or
	-not (Test-Path -LiteralPath (Join-Path $installed 'THIRD_PARTY_NOTICES.md')) -or
	-not (Test-Path -LiteralPath (Join-Path $installed 'licenses') -PathType Container)) {
	throw "The expected Serein release installation was not found at $installed."
}
if (Get-Process -Name 'serein' -ErrorAction SilentlyContinue) {
	throw 'Close Serein before running this update so its executable can be replaced.'
}

if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
	$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
	if (Test-Path -LiteralPath $vswhere) {
		$visualStudio = (& $vswhere -latest -products '*' -property installationPath).Trim()
		if ($LASTEXITCODE -eq 0 -and $visualStudio) {
			$cmakeBin = Join-Path $visualStudio 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin'
			if (Test-Path -LiteralPath (Join-Path $cmakeBin 'cmake.exe')) {
				$env:PATH = "$cmakeBin;$env:PATH"
			}
		}
	}
}
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
	throw 'CMake was not found. Install the CMake component in Visual Studio Build Tools and rerun this script.'
}

$headers = @{ 'User-Agent' = 'Serein-Custom-Nightly-Updater' }
$releases = Invoke-RestMethod -Method Get -Uri 'https://api.github.com/repos/ViceVerse-cz/Serein/releases?per_page=100' -Headers $headers
$release = $releases |
	Where-Object {
		-not $_.draft -and $_.prerelease -and
		$_.tag_name -match '^v1\.0\.0-nightly\.\d{8}\.\d+$'
	} |
	Sort-Object -Property published_at -Descending |
	Select-Object -First 1
if (-not $release) {
	throw 'GitHub did not return a published Serein nightly release.'
}
$tag = $release.tag_name
if ($tag -notmatch '^v1\.0\.0-nightly\.\d{8}\.\d+$') {
	throw 'The latest release tag has an unexpected format; refusing to use it.'
}

Write-Host "Integrating upstream $tag into branch $branch..."
Invoke-Checked -FilePath 'git' -Arguments @('fetch', 'origin', "refs/tags/${tag}:refs/tags/${tag}")
Invoke-Checked -FilePath 'git' -Arguments @('merge', '--no-edit', $tag)

Write-Host 'Running the repository checks...'
Invoke-Checked -FilePath 'cargo' -Arguments @('xtask', 'check')

Write-Host 'Building the customized release with fat LTO...'
Invoke-Checked -FilePath 'cargo' -Arguments @('build', '--locked', '--release', '-p', 'serein', '--no-default-features', '--features', 'custom-build')

$binary = Join-Path $repository 'target\release\serein.exe'
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
	throw "The release build did not produce $binary."
}
Copy-Item -LiteralPath $binary -Destination (Join-Path $installed 'serein.exe') -Force
$hash = (Get-FileHash -LiteralPath (Join-Path $installed 'serein.exe') -Algorithm SHA256).Hash
Write-Host "Installed customized $tag at $installed\serein.exe"
Write-Host "SHA-256: $hash"
