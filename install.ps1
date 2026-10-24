# whalegate installer for Windows (PowerShell 5.1+).
#
#   irm https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.ps1 | iex
#
# What it does:
#   1. downloads the prebuilt binary into %LOCALAPPDATA%\whalegate (checksum-verified) and adds it to PATH
#   2. starts it hidden in the background and on every login (Startup folder shortcut, no admin needed)
#   3. points dsh at it, if dsh is installed
#   4. opens the dashboard - add a DeepSeek account there
#
# Options (environment variables): WHALEGATE_VERSION (e.g. v0.4.0), WHALEGATE_PORT (default 8790),
#   WHALEGATE_NO_SERVICE=1 (binary only), WHALEGATE_NO_DSH=1 (don't touch dsh)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo    = 'RuriMeiko/whalegate'
$Version = if ($env:WHALEGATE_VERSION) { $env:WHALEGATE_VERSION } else { 'latest' }
$Port    = if ($env:WHALEGATE_PORT) { $env:WHALEGATE_PORT } else { '8790' }
$Dir     = Join-Path $env:LOCALAPPDATA 'whalegate'
$Exe     = Join-Path $Dir 'whalegate.exe'
$Target  = 'x86_64-pc-windows-msvc'   # also runs on Windows on ARM via emulation
$Archive = "whalegate-$Target.zip"
$Base    = if ($Version -eq 'latest') { "https://github.com/$Repo/releases/latest/download" } else { "https://github.com/$Repo/releases/download/$Version" }

function Say($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

$Tmp = Join-Path ([IO.Path]::GetTempPath()) ("whalegate-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $Tmp | Out-Null
try {
    Say "Downloading $Archive ($Version)"
    Invoke-WebRequest -UseBasicParsing -Uri "$Base/$Archive" -OutFile (Join-Path $Tmp $Archive)

    $sums = Join-Path $Tmp 'SHA256SUMS.txt'
    $haveSums = $true
    try { Invoke-WebRequest -UseBasicParsing -Uri "$Base/SHA256SUMS.txt" -OutFile $sums } catch { $haveSums = $false }
    if ($haveSums) {
        $line = Get-Content $sums | Where-Object { $_ -match " $([regex]::Escape($Archive))$" } | Select-Object -First 1
        $want = ($line -split '\s+')[0]
        $got = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp $Archive)).Hash
        if (-not $want -or $want.ToLower() -ne $got.ToLower()) { throw "checksum mismatch for $Archive" }
        Say 'Checksum OK'
    } else { Say 'No checksum file published - skipping verification' }

    # stop a running copy so the exe can be replaced
    Get-Process whalegate -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500

    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    Expand-Archive -Force -Path (Join-Path $Tmp $Archive) -DestinationPath $Dir
    Say "Installed $(& $Exe version) -> $Exe"
} finally {
    Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}

# PATH (current user)
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not ($userPath -split ';' | Where-Object { $_ -eq $Dir })) {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$Dir").TrimStart(';'), 'User')
    Say "Added $Dir to your PATH (new terminals will see 'whalegate')"
}
$env:Path = "$env:Path;$Dir"
$env:DS_PORT = $Port

if ($env:WHALEGATE_NO_SERVICE -ne '1') {
    # conhost --headless runs the console app with no window; the Startup shortcut repeats it on every login
    $conhost = Join-Path $env:SystemRoot 'System32\conhost.exe'
    $startup = [Environment]::GetFolderPath('Startup')
    $lnk = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $startup 'whalegate.lnk'))
    $lnk.TargetPath = $conhost
    $lnk.Arguments = "--headless `"$Exe`" serve"
    $lnk.WorkingDirectory = $Dir
    $lnk.WindowStyle = 7
    $lnk.Description = 'whalegate gateway'
    $lnk.Save()
    # DS_PORT for the login-time copy too
    [Environment]::SetEnvironmentVariable('DS_PORT', $Port, 'User')

    Start-Process -FilePath $conhost -ArgumentList '--headless', "`"$Exe`"", 'serve' -WorkingDirectory $Dir -WindowStyle Hidden
    Say 'Started in the background and added to Startup'

    $up = $false
    for ($i = 0; $i -lt 30 -and -not $up; $i++) {
        try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 2 -Uri "http://127.0.0.1:$Port/v1/models" | Out-Null; $up = $true }
        catch { Start-Sleep -Seconds 1 }
    }
    if ($up) { Say "Gateway is up on http://127.0.0.1:$Port" } else { Say "Gateway did not answer yet - run '$Exe serve' in a terminal to see why" }
}

if ($env:WHALEGATE_NO_DSH -ne '1') {
    $dshHome = if ($env:DSH_HOME) { $env:DSH_HOME } else { Join-Path $env:USERPROFILE '.dsh' }
    if (Test-Path (Join-Path $dshHome 'profiles\web')) {
        Say 'Configuring dsh'
        & $Exe setup-dsh
        if ($LASTEXITCODE -ne 0) { Say "dsh setup failed - run 'whalegate setup-dsh' later, or use the dsh button on the dashboard" }
    }
}

Write-Host ''
Write-Host '  whalegate is installed.'
Write-Host ''
Write-Host "  1. Dashboard:  http://127.0.0.1:$Port/   (opening it now)"
Write-Host '  2. Add a DeepSeek account (Chrome login or paste a token)'
Write-Host "  3. Point any OpenAI client at  http://127.0.0.1:$Port/v1  with model `"deepseek-web`""
Write-Host ''
if ($env:WHALEGATE_NO_SERVICE -ne '1') { Start-Process "http://127.0.0.1:$Port/" }
