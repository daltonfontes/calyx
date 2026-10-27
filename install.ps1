# Installs calyx.exe from the GitHub releases, on Windows (no WSL needed).
#
#   irm https://raw.githubusercontent.com/daltonfontes/calyx/main/install.ps1 | iex
#
# The binary is self-contained: no Rust, no C compiler, no DLLs beyond
# Windows' own. Variables, as for install.sh:
#   CALYX_VERSION          a release tag, e.g. v0.4.0 (default: the latest)
#   CALYX_INSTALL          where to install (default: ~\.calyx); the binary
#                          goes in $CALYX_INSTALL\bin, and a copy of it stays
#                          in $CALYX_INSTALL\versions\<version>\, so that the
#                          runs this version starts finish on it after an
#                          upgrade (D23; docs/operacao.md)
#   CALYX_BASE_URL         where releases are downloaded from (mirrors, tests)
#   CALYX_NO_MODIFY_PATH   1: do not add the bin directory to the user's PATH
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'daltonfontes/calyx'
$version = if ($env:CALYX_VERSION) { $env:CALYX_VERSION } else { 'latest' }
$dir = if ($env:CALYX_INSTALL) { $env:CALYX_INSTALL } else { Join-Path $HOME '.calyx' }

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'calyx install: no prebuilt binary for 32-bit Windows'
}
# x64 binaries also run on Windows on ARM, emulated.
$target = 'x86_64-pc-windows-gnu'
$archive = "calyx-$target.zip"
$base = if ($env:CALYX_BASE_URL) { $env:CALYX_BASE_URL }
        elseif ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" }
        else { "https://github.com/$repo/releases/download/$version" }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("calyx-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "downloading $archive ($version)"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive" -OutFile (Join-Path $tmp $archive)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$archive.sha256" -OutFile (Join-Path $tmp "$archive.sha256")

    $expected = ((Get-Content (Join-Path $tmp "$archive.sha256") -Raw).Trim() -split '\s+')[0].ToLower()
    $actual = (Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        throw "calyx install: checksum mismatch for $archive (expected $expected, got $actual)"
    }

    Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath (Join-Path $tmp 'x')
    $new = Join-Path $tmp 'x\calyx.exe'
    $bin = Join-Path $dir 'bin'
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    $exe = Join-Path $bin 'calyx.exe'
    # A running calyx.exe cannot be overwritten, but it can be renamed.
    if (Test-Path $exe) { Move-Item -Force $exe "$exe.old" }
    Copy-Item $new $exe
    Remove-Item -Force "$exe.old" -ErrorAction SilentlyContinue

    # Each version stays: `calyx resume` hands a run to the version that
    # started it, if a newer one compiles its program differently.
    $installed = ((& $exe --version) -split ' ')[1]
    $kept = Join-Path $dir "versions\$installed"
    New-Item -ItemType Directory -Force -Path $kept | Out-Null
    Copy-Item -Force $new (Join-Path $kept 'calyx.exe')

    Write-Host "installed $(& $exe --version) in $exe"
    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($path -split ';') -contains $bin)) {
        if ($env:CALYX_NO_MODIFY_PATH -eq '1') {
            Write-Host ''
            Write-Host "add $bin to your PATH"
        } else {
            [Environment]::SetEnvironmentVariable('Path', ((@($path, $bin) | Where-Object { $_ }) -join ';'), 'User')
            Write-Host "added $bin to your PATH (open a new terminal to use it)"
        }
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
