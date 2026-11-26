# Test the Windows setup .exe through its whole life, as a user would live it:
#
#   1. install the previous release, start it, and update by running the new
#      setup.exe WHILE THE OLD ONE IS RUNNING (the common case: the in-app
#      updater and a manual download both do exactly that)
#   2. the new version runs (--version) and launches
#   3. run the setup again over itself: a repair, nothing lost
#   4. uninstall from Add/Remove Programs (its registered uninstaller): the
#      app, its entry and its shortcuts go, the user's data stays
#   5. install again, then the app's own full uninstall (--uninstall, what
#      Settings -> Uninstall runs): everything goes, data included
#
# Each step is checked against what Windows records, not just exit codes:
# the Uninstall registry entry (one, at the right version), the files, the
# Start Menu and Desktop shortcuts and what they point at.
#
#   test-install-windows.ps1 -New <setup.exe> -Version <x.y.z> [-Prev <setup.exe> -PrevVersion <x.y.z>]
param(
    [Parameter(Mandatory)][string]$New,
    [Parameter(Mandatory)][string]$Version,
    [string]$Prev = '',
    [string]$PrevVersion = ''
)
$ErrorActionPreference = 'Stop'
$AppName = 'Soul Player'
$ExeName = 'soul-player.exe'
# dirs::config_dir() and dirs::cache_dir()/data_local_dir() on Windows.
$Data = Join-Path $env:APPDATA 'soul-player'
$LocalData = Join-Path $env:LOCALAPPDATA 'soul-player'
$Marker = Join-Path $Data 'user-data.marker'
$env:SOUL_PLAYER_NO_UPDATE = '1'   # the launched app must not update itself mid-test

function Fail([string]$m) { Write-Host "FAIL: $m"; exit 1 }
function Ok([string]$m) { Write-Host "ok: $m" }
function Step([string]$m) { Write-Host "`n== $m" }

function Get-Entries {
    $roots = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
             'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
             'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
    foreach ($r in $roots) {
        if (-not (Test-Path $r)) { continue }
        Get-ChildItem $r | ForEach-Object { Get-ItemProperty $_.PSPath } |
            Where-Object { $_.DisplayName -eq $AppName }
    }
}

function Get-Entry {
    $e = @(Get-Entries)
    if ($e.Count -ne 1) { Fail "expected exactly one '$AppName' in Add/Remove Programs, found $($e.Count)" }
    $e[0]
}

function Get-InstallDir {
    $u = (Get-Entry).UninstallString.Trim()
    if ($u.StartsWith('"')) { $u = $u.Substring(1, $u.IndexOf('"', 1) - 1) }
    Split-Path $u -Parent
}

function Get-Shortcuts {
    $places = (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'),
              (Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs'),
              [Environment]::GetFolderPath('Desktop'),
              [Environment]::GetFolderPath('CommonDesktopDirectory')
    foreach ($p in $places) {
        if ($p -and (Test-Path $p)) { Get-ChildItem $p -Recurse -Filter "$AppName.lnk" -ErrorAction SilentlyContinue }
    }
}

function Wait-Until([scriptblock]$Pred, [int]$Seconds) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Pred) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return [bool](& $Pred)
}

function Install-Setup([string]$Setup) {
    Write-Host "> $(Split-Path $Setup -Leaf) /S"
    $p = Start-Process -FilePath $Setup -ArgumentList '/S' -Wait -PassThru
    if ($p.ExitCode -ne 0) { Fail "$(Split-Path $Setup -Leaf) /S exited $($p.ExitCode)" }
}

function Assert-Installed([string]$Want) {
    $e = Get-Entry
    if ($e.DisplayVersion -ne $Want) { Fail "Add/Remove Programs says $($e.DisplayVersion), expected $Want" }
    if (-not $e.Publisher) { Fail "the Add/Remove Programs entry has no publisher" }
    $dir = Get-InstallDir
    $exe = Join-Path $dir $ExeName
    if (-not (Test-Path $exe)) { Fail "no $ExeName in $dir" }
    if (Test-Path (Join-Path $dir 'soul-player-rune.exe')) { Fail "the pre-0.4.3 soul-player-rune.exe is still in $dir" }
    $links = @(Get-Shortcuts)
    if ($links.Count -lt 1) { Fail "no '$AppName' shortcut in the Start Menu or on the Desktop" }
    $shell = New-Object -ComObject WScript.Shell
    foreach ($l in $links) {
        $target = $shell.CreateShortcut($l.FullName).TargetPath
        if ($target -ne $exe) { Fail "shortcut $($l.FullName) points at '$target', not $exe" }
    }
    Ok "installed $Want in $dir; Add/Remove Programs and $($links.Count) shortcut(s) agree"
    $exe
}

function Assert-Runs([string]$Exe, [string]$Want) {
    $out = (& $Exe --version 2>$null | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { Fail "$ExeName --version exited $LASTEXITCODE" }
    if ($out -ne "soul-player $Want") { Fail "--version printed '$out', expected 'soul-player $Want'" }
    Ok "it runs: $out"
}

function Start-App([string]$Exe) {
    $p = Start-Process -FilePath $Exe -PassThru
    Start-Sleep -Seconds 8
    if ($p.HasExited) { Fail "the app exited at launch (code $($p.ExitCode))" }
    $p
}

function Assert-DataKept([string]$After) {
    if (-not (Test-Path $Marker)) { Fail "$After deleted the user's data in $Data" }
    Ok "the user's data survived $After"
}

function Uninstall-FromControlPanel {
    $u = (Get-Entry).UninstallString.Trim()
    $exe = if ($u.StartsWith('"')) { $u.Substring(1, $u.IndexOf('"', 1) - 1) } else { $u }
    $dir = Get-InstallDir
    Write-Host "> $exe /S"
    Start-Process -FilePath $exe -ArgumentList '/S' -Wait
    # NSIS copies its uninstaller to %TEMP% and returns at once: poll.
    if (-not (Wait-Until { @(Get-Entries).Count -eq 0 -and -not (Test-Path (Join-Path $dir $ExeName)) } 180)) {
        Fail "uninstall left: $(@(Get-Entries).Count) Add/Remove entries, exe present: $(Test-Path (Join-Path $dir $ExeName))"
    }
    $dir
}

function Assert-Removed([string]$Dir) {
    if (@(Get-Entries).Count -ne 0) { Fail "Add/Remove Programs still lists $AppName" }
    if ((Test-Path $Dir) -and @(Get-ChildItem $Dir -Force).Count -gt 0) {
        Fail "files left in $Dir`: $((Get-ChildItem $Dir -Force).Name -join ', ')"
    }
    $links = @(Get-Shortcuts)
    if ($links.Count -gt 0) { Fail "shortcuts left: $($links.FullName -join ', ')" }
    Ok "the app, its Add/Remove entry and its shortcuts are gone"
}

# Nothing of a previous install, so every check below is about this run.
if (@(Get-Entries).Count -ne 0) { Fail "$AppName is already installed on this runner" }

# ----------------------------------------------------------------- 1. update
if ($Prev -and $PrevVersion -and $PrevVersion -ne $Version) {
    Step "1a. install the previous release, $PrevVersion"
    Install-Setup $Prev
    $exe = Assert-Installed $PrevVersion
    New-Item -ItemType Directory -Force $Data | Out-Null
    Set-Content $Marker 'kept'

    Step "1b. update to $Version while $PrevVersion is running"
    $old = Start-App $exe
    Install-Setup $New
    # The installer closes the running app rather than failing to replace it.
    if (-not (Wait-Until { $old.HasExited } 30)) { Fail "the running $PrevVersion was not closed by the update" }
    Ok "the running $PrevVersion was closed for the update"
    $exe = Assert-Installed $Version
    Assert-Runs $exe $Version
    Assert-DataKept 'the update'
}
else {
    Step "1. install $Version"
    Install-Setup $New
    $exe = Assert-Installed $Version
    Assert-Runs $exe $Version
    New-Item -ItemType Directory -Force $Data | Out-Null
    Set-Content $Marker 'kept'
}

# ----------------------------------------------------------------- 2. launch
Step "2. launch"
$app = Start-App $exe
Ok "the app launches"
Stop-Process -Id $app.Id -Force
$null = Wait-Until { $app.HasExited } 15

# ----------------------------------------------------------------- 3. repair
Step "3. run the same setup again"
Install-Setup $New
$exe = Assert-Installed $Version
Assert-Runs $exe $Version
Assert-DataKept 'a second install'

# -------------------------------------------------------------- 4. uninstall
Step "4. uninstall from Add/Remove Programs"
$dir = Uninstall-FromControlPanel
Assert-Removed $dir
Assert-DataKept 'the uninstaller'

# ----------------------------------------------------------- 5. full removal
Step "5. install again, then the app's full uninstall (--uninstall)"
Install-Setup $New
$exe = Assert-Installed $Version
$dir = Split-Path $exe -Parent
& $exe --uninstall
if ($LASTEXITCODE -ne 0) { Fail "$ExeName --uninstall exited $LASTEXITCODE" }
# It removes the data itself and hands the app to the registered uninstaller,
# which finishes once this process is gone.
if (-not (Wait-Until { @(Get-Entries).Count -eq 0 -and -not (Test-Path $exe) } 180)) {
    Fail "after --uninstall: $(@(Get-Entries).Count) Add/Remove entries, exe present: $(Test-Path $exe)"
}
Assert-Removed $dir
foreach ($d in $Data, $LocalData) {
    if (Test-Path $d) { Fail "--uninstall left $d" }
}
Ok "--uninstall removed the settings and data too"

Write-Host "`nPASS: Windows setup.exe $Version$(if ($PrevVersion -and $Prev) { " (from $PrevVersion)" }): install, update while running, repair, uninstall, full uninstall"
