# Releases a version: scripts\release.ps1 0.2.0
#
# Write the changes under "## Unreleased" in CHANGELOG.md as you go. This script renames that
# heading to the version and today's date, sets the version in Cargo.toml and Cargo.lock,
# commits, pushes, then tags. The tag starts .github/workflows/release.yml, which builds, packs
# and publishes the release. A version with a dash, like 0.2.0-beta.1, becomes a pre-release.
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)

if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') { throw "$Version isn't a version like 1.2.3." }

# A release must match code that is committed and pushed.
if (git status --porcelain) { throw 'Working tree has uncommitted changes. Commit or stash them first.' }
if ((git branch --show-current) -ne 'master') { throw 'Release from master.' }
git fetch --quiet
if ($LASTEXITCODE) { throw 'git fetch failed' }
if (git rev-list HEAD..origin/master) { throw 'origin/master has commits you lack. Pull first.' }
if (git tag --list "v$Version") { throw "v$Version is tagged already." }

$changelog = [IO.File]::ReadAllText("$PWD\CHANGELOG.md")
if ($changelog -notmatch '(?m)^## Unreleased\r?\n\s*\S') { throw 'CHANGELOG.md has nothing under "## Unreleased".' }
# WriteAllText writes UTF-8 without a byte order mark, which Cargo and GitHub expect.
$changelog = $changelog -replace '(?m)^## Unreleased', "## $Version ($(Get-Date -Format yyyy-MM-dd))"
[IO.File]::WriteAllText("$PWD\CHANGELOG.md", $changelog)
# Only the first match: the package's own version.
$toml = ([regex]'(?m)^version = ".+"').Replace([IO.File]::ReadAllText("$PWD\Cargo.toml"), "version = `"$Version`"", 1)
[IO.File]::WriteAllText("$PWD\Cargo.toml", $toml)
cargo metadata --offline --format-version 1 | Out-Null # updates gifly's version in Cargo.lock
if ($LASTEXITCODE) { throw 'cargo metadata failed' }

git commit --quiet -m "chore(release): $Version" -- CHANGELOG.md Cargo.toml Cargo.lock
git push --quiet origin master
if ($LASTEXITCODE) { throw 'Pushing master failed' }
git tag "v$Version"
git push --quiet origin "v$Version"
if ($LASTEXITCODE) { throw 'Pushing the tag failed' }
Write-Host "Released v$Version. Follow the build under Actions, Release."
