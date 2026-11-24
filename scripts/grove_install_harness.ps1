# Isolated lifecycle harness for scripts/grove-install-common.ps1, run by scripts/test_grove_beta.py.
# Ported from Tide's scripts/test_windows_install.py (yotam1001/tide-music@4bc2ba3) and owned by Grove.
# It fakes the registry, shortcuts and processes; it never installs, registers or launches anything real.
param($Common, $Fixture, $Scenario)
$ErrorActionPreference = 'Stop'
. $Common
$root = Join-Path $Fixture 'installed'
$links = Join-Path $Fixture 'start-menu'
$package = Join-Path $Fixture 'package'
$session = Join-Path $Fixture 'saved-session-sentinel'
[IO.File]::WriteAllText($session, 'synthetic unchanged data')
$global:MockProcesses = $null
$global:LateProcesses = $null
$global:ReopenedProcesses = @{}
$global:Busy = $false
$global:FailDependency = $false
$global:FailShortcut = $false
$global:FailRegistration = $false
$global:Registry = @{}
$global:AppsEntry = $null
$global:AppWrites = 0
$global:FailAppWrite = 0
$global:RaceNewAppEntry = $false
function Get-GroveUninstallEntry {
    if ($null -eq $global:AppsEntry) { return $null }
    $copy = @{}; foreach ($name in $global:AppsEntry.get_Keys()) { $copy[$name] = $global:AppsEntry[$name] }
    return $copy
}
function New-GroveUninstallEntry {
    if ($global:RaceNewAppEntry) { $global:AppsEntry = @{Foreign='preserve'}; throw 'Synthetic concurrent entry claim' }
    if ($null -ne $global:AppsEntry) { throw 'Synthetic entry already exists' }
    $global:AppsEntry = @{}
}
function Write-GroveUninstallValue {
    param($Name, $Value)
    $global:AppWrites++
    if ($global:FailAppWrite -eq $global:AppWrites) { $global:FailAppWrite = 0; throw 'Synthetic uninstall registry write failure' }
    $global:AppsEntry[$Name] = $Value
}
function Remove-GroveUninstallValue { param($Name) $global:AppsEntry.Remove($Name) }
function Remove-GroveUninstallEntryKey { $global:AppsEntry = $null }
function Get-GroveCloseCandidate { param($Id) if ($global:ReopenedProcesses.ContainsKey($Id)) { return $global:ReopenedProcesses[$Id] }; return @($global:MockProcesses | Where-Object Id -EQ $Id)[0] }
function Get-GroveOwnedProcesses { param($InstallRoot) if ($null -ne $global:MockProcesses) { return @($global:MockProcesses | Where-Object { !$_.HasExited }) }; if ($global:Busy) { return @('synthetic owned process') }; return @() }
function Get-GroveShortcut {
    param($Path)
    if (Test-Path -LiteralPath $Path) { return Read-GroveJson $Path }
    return $null
}
function Set-GroveShortcut {
    param($Path, $Value)
    if ($global:FailShortcut) { $global:FailShortcut = $false; throw 'Synthetic shortcut failure' }
    Write-GroveJson $Path $Value
}
function Invoke-GroveDependencies {
    param($Release)
    $tools = Join-Path $Release 'tools'
    [IO.Directory]::CreateDirectory($tools) | Out-Null
    [IO.File]::WriteAllText((Join-Path $tools 'mpv.exe'), 'synthetic tool, never executed')
    if ($null -ne $global:LateProcesses) { $global:MockProcesses = $global:LateProcesses; $global:LateProcesses = $null }
    if ($global:FailDependency) { throw 'Synthetic dependency failure' }
    $python = Join-Path $Release '.tools/provider-venv/Scripts'
    [IO.Directory]::CreateDirectory($python) | Out-Null
    [IO.File]::WriteAllText((Join-Path $python 'python.exe'), 'synthetic interpreter, never executed')
}
function Invoke-GroveBrowserSetup {
    param($Release, $Mode, $Browser, $ExtensionId)
    if ($Browser -eq 'None') { return }
    switch ($Mode) {
        'Prepare' {
            $folder = Join-Path $Release 'native-host'
            [IO.Directory]::CreateDirectory($folder) | Out-Null
            Write-GroveJson (Join-Path $folder 'fixture-host.json') @{Path=$Release; Id=$ExtensionId}
        }
        'Register' {
            if ($global:FailRegistration) { $global:FailRegistration = $false; throw 'Synthetic registration failure' }
            if ($global:Registry.ContainsKey($Browser) -and $global:Registry[$Browser] -ne $Release) { throw 'Conflicting synthetic registration' }
            $global:Registry[$Browser] = $Release
        }
        'Unregister' {
            if ($global:Registry.ContainsKey($Browser) -and $global:Registry[$Browser] -ne $Release) { throw 'Changed synthetic registration' }
            $global:Registry.Remove($Browser)
        }
    }
}
function Must-Fail([scriptblock]$Action) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    if (!$failed) { throw 'Expected fixture failure was not observed' }
}
function New-Install { Install-Grove $package $root $links 'None' '' | Out-Null }
function Current-Release { $state = Get-GroveInstallState $root; return Join-Path $root ('releases/' + $state.Release) }
function New-MockPlayer([string]$Path, [string]$Name = 'grove-music') {
    $p = [pscustomobject]@{Id=123; StartTime=[DateTime]::UtcNow; Path=$Path; ProcessName=$Name; SessionId=[Diagnostics.Process]::GetCurrentProcess().SessionId; HasExited=$false; Handle=[IntPtr]::Zero; CloseCount=0}
    $p | Add-Member -MemberType ScriptMethod -Name CloseMainWindow -Value { $this.CloseCount++; $this.HasExited=$true; return $true }
    $p | Add-Member -MemberType ScriptMethod -Name Dispose -Value { }
    return $p
}
switch ($Scenario) {
    'multiple-player-update' {
        New-Install; $previous = Current-Release
        $first = New-MockPlayer (Join-Path $previous 'grove-music.exe')
        $second = New-MockPlayer (Join-Path $previous 'grove-music.exe')
        $second.Id = 124
        $second | Add-Member -Force -MemberType ScriptMethod -Name CloseMainWindow -Value { $this.CloseCount++; return $false }
        $global:HiddenCloseCount = 0
        function Request-GroveHiddenWindowClose([int]$Id) {
            if ($Id -ne $second.Id) { throw 'Unexpected hidden window target' }
            $global:HiddenCloseCount++
            $second.HasExited = $true
            return $true
        }
        $global:MockProcesses = @($first,$second)
        Test-GroveInstall $package $root $links 'None' '' '1.2.3' | Out-Null
        if ($first.CloseCount -or $second.CloseCount -or $global:HiddenCloseCount) { throw 'Preflight closed a player before COMMIT' }
        Install-Grove $package $root $links 'None' '' '1.2.3' -CloseRunningPlayers | Out-Null
        if ($first.CloseCount -ne 1 -or $second.CloseCount -ne 1 -or $global:HiddenCloseCount -ne 1 -or
            (Current-Release) -eq $previous) { throw 'Visible and hidden windows did not close gracefully for update' }
        $global:MockProcesses = $null
        Uninstall-Grove $root $links | Out-Null
    }
    'late-player-update' {
        New-Install; $previous = Current-Release
        $late = New-MockPlayer (Join-Path $previous 'grove-music.exe')
        $global:MockProcesses = @(); $global:LateProcesses = @($late)
        Install-Grove $package $root $links 'None' '' '1.2.3' -CloseRunningPlayers | Out-Null
        if ($late.CloseCount -ne 1 -or (Current-Release) -eq $previous) { throw 'A player opened during downloads blocked update' }
        $global:MockProcesses = $null
        Uninstall-Grove $root $links | Out-Null
    }
    'close-player-boundaries' {
        New-Install; $release = Current-Release
        foreach ($mode in @('outside','helper','session','unowned','tampered')) {
            $p = New-MockPlayer (Join-Path $release 'grove-music.exe')
            switch ($mode) {
                'outside' { $p.Path = Join-Path $Fixture 'grove-music.exe' }
                'helper' { $p.ProcessName = 'mpv' }
                'session' { $p.SessionId++ }
                'unowned' { $p.Path = Join-Path $root ('releases/' + ('c' * 32) + '/grove-music.exe') }
                'tampered' { [IO.File]::AppendAllText((Join-Path $release 'grove-music.exe'), 'tampered') }
            }
            $global:MockProcesses = @($p)
            Must-Fail { Wait-GroveStopped $root 0 }
            if ($p.CloseCount -or $p.HasExited) { throw ('Unexpected player close: ' + $mode) }
        }
        [IO.File]::Copy((Join-Path $package 'grove-music.exe'), (Join-Path $release 'grove-music.exe'), $true)
        $stale = New-MockPlayer (Join-Path $release 'grove-music.exe')
        $recycled = New-MockPlayer $stale.Path
        $recycled.StartTime = $stale.StartTime.AddSeconds(1)
        $global:MockProcesses = @($stale); $global:ReopenedProcesses[$stale.Id] = $recycled
        Must-Fail { Wait-GroveStopped $root 0 }
        if ($stale.CloseCount -or $recycled.CloseCount -or $recycled.HasExited) { throw 'A recycled PID inherited stale close authorization' }
        $global:ReopenedProcesses = @{}
        $global:MockProcesses = $null
        # Restore fixture payload so ordinary owned cleanup remains verifiable.
        [IO.File]::Copy((Join-Path $package 'grove-music.exe'), (Join-Path $release 'grove-music.exe'), $true)
        Uninstall-Grove $root $links | Out-Null
    }
    'preflight' {
        $events = @(Test-GroveInstall $package $root $links 'None' '' '1.2.3')
        if ($events[-1] -ne 'GROVE_PREFLIGHT_OK' -or (Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $links)) { throw 'Fresh preflight wrote an installation' }
        New-Install; $previous = Current-Release; $entry = Get-GroveUninstallEntry
        $global:Busy = $true; $global:FailDependency = $true
        $events = @(Test-GroveInstall $package $root $links 'None' '' '1.2.3')
        if ($events[-1] -ne 'GROVE_PREFLIGHT_OK' -or (Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $entry)) { throw 'Running-player preflight changed ownership or registration' }
        $global:AppsEntry.UninstallString = 'different.exe'
        Must-Fail { Test-GroveInstall $package $root $links 'None' '' '1.2.3' }
        if ($global:AppsEntry.UninstallString -ne 'different.exe') { throw 'Conflicting preflight changed registration' }
        $global:AppsEntry = $entry; $global:Busy = $false; $global:FailDependency = $false
        Uninstall-Grove $root $links | Out-Null
    }
    'fresh-remove' {
        New-Install
        $release = Current-Release
        if (!(Test-Path -LiteralPath (Join-Path $links 'Grove.lnk'))) { throw 'Missing registration/shortcut' }
        $expected = Get-GroveUninstallRegistration $release $root '0.1.0-beta.1'
        if (!(Test-GroveUninstallEntryMatch $global:AppsEntry $expected) -or $global:AppsEntry.DisplayName -ne 'Grove' -or $global:AppsEntry.NoModify -ne 1 -or $global:AppsEntry.NoRepair -ne 1) { throw 'Missing Windows Settings uninstall fields' }
        Uninstall-Grove $root $links | Out-Null
        if ((Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $links) -or $global:Registry.Count -or $null -ne $global:AppsEntry) { throw 'Owned removal incomplete' }
        Uninstall-Grove $root $links | Out-Null
    }
    'upgrade' {
        New-Install; $previous = Current-Release
        Install-Grove $package $root $links 'None' '' | Out-Null
        $current = Current-Release
        if ($current -eq $previous -or (Test-Path -LiteralPath $previous)) { throw 'Upgrade failed to replace owned release' }
        Uninstall-Grove $root $links | Out-Null
    }
    'preserve-extra' {
        New-Install; $previous = Current-Release
        [IO.File]::WriteAllText((Join-Path $previous 'user-extra.txt'), 'keep')
        Install-Grove $package $root $links 'None' '' | Out-Null
        Uninstall-Grove $root $links | Out-Null
        if ([IO.File]::ReadAllText((Join-Path $previous 'user-extra.txt')) -ne 'keep') { throw 'Removed unowned file' }
    }
    'dependency-failure-first' {
        $global:FailDependency = $true; Must-Fail { New-Install }
        if ((Test-Path -LiteralPath $root) -or (Test-Path -LiteralPath $links) -or $global:Registry.Count) { throw 'Failed fresh install left owned output' }
    }
    { $_ -in @('dependency-failure-update','shortcut-failure','registration-failure') } {
        New-Install; $previous = Current-Release
        $before = [IO.File]::ReadAllText((Join-Path $links 'Grove.lnk'))
        if ($Scenario -eq 'dependency-failure-update') { $global:FailDependency = $true }
        if ($Scenario -eq 'shortcut-failure') { $global:FailShortcut = $true }
        if ($Scenario -eq 'registration-failure') { $global:FailRegistration = $true }
        Must-Fail { New-Install }
        if ((Current-Release) -ne $previous -or [IO.File]::ReadAllText((Join-Path $links 'Grove.lnk')) -ne $before) { throw 'Failed update did not restore previous install' }
        if (@(Get-ChildItem -LiteralPath (Join-Path $root 'releases')).Count -ne 1) { throw 'Failed release was retained' }
        Uninstall-Grove $root $links | Out-Null
    }
    'busy' {
        New-Install; $previous = Current-Release; $global:Busy = $true
        Must-Fail { New-Install }; Must-Fail { Uninstall-Grove $root $links }
        if ((Current-Release) -ne $previous) { throw 'Busy install changed' }
        $global:Busy = $false; Uninstall-Grove $root $links | Out-Null
    }
    'unknown-root' {
        [IO.Directory]::CreateDirectory($root) | Out-Null
        $sentinel = Join-Path $root 'unrelated.txt'; [IO.File]::WriteAllText($sentinel, 'keep')
        Must-Fail { New-Install }; Must-Fail { Uninstall-Grove $root $links }
        if ([IO.File]::ReadAllText($sentinel) -ne 'keep') { throw 'Unrelated install changed' }
    }
    'shortcut-conflict' {
        [IO.Directory]::CreateDirectory($links) | Out-Null
        Write-GroveJson (Join-Path $links 'Grove.lnk') @{Target='different.exe'; Arguments=''}
        Must-Fail { New-Install }
        if ((Test-Path -LiteralPath $root) -or (Get-GroveShortcut (Join-Path $links 'Grove.lnk')).Target -ne 'different.exe') { throw 'Conflicting shortcut changed' }
    }
    'registration-conflict' {
        $global:Registry['Chrome'] = 'unrelated installed helper'; Must-Fail { New-Install }
        if ($global:Registry['Chrome'] -ne 'unrelated installed helper' -or (Test-Path -LiteralPath $root)) { throw 'Conflicting registration changed' }
    }
    'changed-shortcut-removal' {
        New-Install
        Write-GroveJson (Join-Path $links 'Grove.lnk') @{Target='different.exe'; Arguments=''}
        Uninstall-Grove $root $links | Out-Null
        if ((Get-GroveShortcut (Join-Path $links 'Grove.lnk')).Target -ne 'different.exe') { throw 'Changed shortcut removed' }
    }
    'changed-registration-removal' {
        New-Install; $previous = Current-Release
        $global:Registry['Chrome'] = 'different helper'; Must-Fail { Uninstall-Grove $root $links }
        if ((Current-Release) -ne $previous -or $global:Registry['Chrome'] -ne 'different helper') { throw 'Changed registration removed' }
    }
    'corrupt-package' {
        [IO.File]::WriteAllText((Join-Path $package 'grove-music.exe'), 'corrupted')
        Must-Fail { New-Install }
        if (Test-Path -LiteralPath $root) { throw 'Corrupt package wrote installation' }
    }
    'traversal-record' {
        New-Install; $release = Current-Release
        Write-GroveJson (Join-Path $release '.grove-release.json') @{Product='org.grovemusic.windows.beta'; Files=@('../../../../saved-session-sentinel')}
        Must-Fail { Uninstall-Grove $root $links }
        if (!(Test-Path -LiteralPath (Join-Path $release 'grove-music.exe'))) { throw 'Invalid ownership deleted files' }
    }
    'reparse' {
        $outside = Join-Path $Fixture 'outside'; [IO.Directory]::CreateDirectory($outside) | Out-Null
        New-Item -ItemType Junction -Path $root -Target $outside | Out-Null
        Must-Fail { New-Install }; Must-Fail { Uninstall-Grove $root $links }
        if (@(Get-ChildItem -LiteralPath $outside).Count) { throw 'Reparse target changed' }
        Remove-Item -LiteralPath $root
    }
    'lock' {
        New-Install; $previous = Current-Release
        $stream = [IO.File]::Open((Join-Path $root '.grove-install.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
        try { Must-Fail { New-Install }; Must-Fail { Uninstall-Grove $root $links } } finally { $stream.Dispose() }
        if ((Current-Release) -ne $previous) { throw 'Locked install changed' }
        Uninstall-Grove $root $links | Out-Null
    }
    'download' {
        $global:Downloads = 0
        function Invoke-WebRequest { param([switch]$UseBasicParsing, $Uri, $OutFile, $TimeoutSec) if ($TimeoutSec -ne 300) { throw 'Missing bounded download timeout' }; $global:Downloads++; [IO.File]::WriteAllText($OutFile, 'fixture') }
        Must-Fail { Get-GroveDownload 'https://example.invalid/fixture' '' (Join-Path $Fixture 'missing') }
        if ($global:Downloads) { throw 'Missing digest started download' }
        Must-Fail { Get-GroveDownload 'https://example.invalid/fixture' ('0' * 64) (Join-Path $Fixture 'bad') }
        if ($global:Downloads -ne 1) { throw 'Download checksum fixture did not run' }
    }
    'archive-paths' {
        foreach ($name in @('traversal','rooted','duplicate')) {
            Must-Fail { Expand-GroveArchive (Join-Path $Fixture ($name + '.zip')) (Join-Path $Fixture $name) }
            if (Test-Path -LiteralPath (Join-Path $Fixture $name)) { throw 'Invalid archive wrote files' }
        }
    }
    'runtime-api-independent' {
        $global:MetadataRequests = 0
        $global:Downloads = 0
        function Invoke-RestMethod { $global:MetadataRequests++; throw 'Synthetic GitHub API rate limit' }
        function Invoke-WebRequest {
            param([switch]$UseBasicParsing, $Uri, $OutFile, $TimeoutSec)
            if ($Uri -cne 'https://github.com/mpv-player/mpv/releases/download/v0.41.0/mpv-v0.41.0-x86_64-pc-windows-msvc.zip') { throw 'Unexpected runtime source' }
            if ($TimeoutSec -ne 300) { throw 'Missing bounded download timeout' }
            $global:Downloads++
            [IO.File]::WriteAllBytes($OutFile, [Text.Encoding]::UTF8.GetBytes('fixture'))
        }
        # Load the real asset function without running the bootstrap's other
        # downloads. Common's real checksum/path/overwrite checks remain active.
        $tokens = $null; $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile((Join-Path $Fixture 'source/scripts/setup-runtime.ps1'), [ref]$tokens, [ref]$errors)
        if ($errors.Count) { throw 'Runtime script did not parse' }
        $definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-VerifiedAsset' }, $true)
        if (!$definition) { throw 'Missing production asset function' }
        . ([scriptblock]::Create($definition.Extent.Text))
        $output = Join-Path $Fixture 'verified.zip'
        $hash = 'f16d05ec6b29248d2c61adb1e9263f78e4f7bace1b955014a2d17872cfe4064d'
        Get-VerifiedAsset 'mpv-player/mpv' 'v0.41.0' 'mpv-v0.41.0-x86_64-pc-windows-msvc.zip' $output $hash
        if ([IO.File]::ReadAllText($output) -cne 'fixture' -or $global:Downloads -ne 1 -or $global:MetadataRequests) { throw 'Verified download depended on the API or changed bytes' }
        Must-Fail { Get-VerifiedAsset 'mpv-player/mpv' 'v0.41.0' 'mpv-v0.41.0-x86_64-pc-windows-msvc.zip' $output $hash }
        Must-Fail { Get-VerifiedAsset 'mpv-player/mpv' 'v0.41.0' 'mpv-v0.41.0-x86_64-pc-windows-msvc.zip' (Join-Path $Fixture 'missing.zip') '' }
        if ($global:Downloads -ne 1) { throw 'Missing pin or existing output started a download' }
    }
    'runtime-corrupt-download' {
        $global:MetadataRequests = 0; $global:Downloads = 0
        function Invoke-RestMethod { $global:MetadataRequests++; throw 'Synthetic GitHub API rate limit' }
        function Invoke-WebRequest {
            param([switch]$UseBasicParsing, $Uri, $OutFile, $TimeoutSec)
            $global:Downloads++
            [IO.File]::WriteAllText($OutFile, 'corrupted runtime archive')
        }
        $failure = $null
        try { & (Join-Path $Fixture 'source/scripts/setup-runtime.ps1') -Destination (Join-Path $Fixture 'tools') | Out-Null }
        catch { $failure = $_.Exception.Message }
        if ($failure -cne 'Dependency download checksum failed.') { throw 'Corrupt runtime was not rejected by its actual checksum' }
        if ($global:MetadataRequests -or $global:Downloads -ne 1) { throw 'Corrupt runtime did not reach the checksum check without the API' }
        if (Test-Path -LiteralPath (Join-Path $Fixture 'tools')) { throw 'Corrupt runtime wrote extracted tools' }
    }
    'app-entry-conflict' {
        $global:AppsEntry = @{DisplayName='unrelated product'; UninstallString='unrelated.exe'}
        Must-Fail { New-Install }
        if ((Test-Path -LiteralPath $root) -or $global:AppsEntry.DisplayName -ne 'unrelated product') { throw 'Foreign uninstall entry overwritten' }
    }
    'app-entry-update-conflict' {
        New-Install; $previous = Current-Release
        $global:AppsEntry.UninstallString = 'different.exe'
        Must-Fail { New-Install }; Must-Fail { Uninstall-Grove $root $links }
        if ((Current-Release) -ne $previous -or $global:AppsEntry.UninstallString -ne 'different.exe') { throw 'Changed uninstall entry modified' }
    }
    'app-entry-write-failure-first' {
        $global:FailAppWrite = 3; Must-Fail { New-Install }
        if ((Test-Path -LiteralPath $root) -or $null -ne $global:AppsEntry -or $global:Registry.Count) { throw 'Partial first uninstall entry not rolled back' }
    }
    'app-entry-write-failure-update' {
        New-Install; $previous = Current-Release; $before = Get-GroveUninstallEntry
        $global:FailAppWrite = $global:AppWrites + 3; Must-Fail { New-Install }
        if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch $global:AppsEntry $before)) { throw 'Partial updated uninstall entry not restored' }
        Uninstall-Grove $root $links | Out-Null
    }
    'app-entry-marker-failure' {
        New-Install; $previous = Current-Release; $before = Get-GroveUninstallEntry
        $pending = Join-Path $root '.grove-install.pending.json'; [IO.File]::WriteAllText($pending, 'preserve interrupted record')
        Must-Fail { New-Install }
        if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch $global:AppsEntry $before)) { throw 'Final marker failure did not restore uninstall entry' }
        Remove-Item -LiteralPath $pending
        Uninstall-Grove $root $links | Out-Null
    }
    'app-entry-version-update' {
        New-Install
        Install-Grove $package $root $links 'None' '' '1.2.3-preview' | Out-Null
        $expected = Get-GroveUninstallRegistration (Current-Release) $root '1.2.3-preview'
        if (!(Test-GroveUninstallEntryMatch $global:AppsEntry $expected)) { throw 'Version/uninstall path did not update' }
        Uninstall-Grove $root $links | Out-Null
    }
    { $_ -in @('owned-stale-registration','owned-removed-registration','removed-registration-tampered','removed-registration-unowned','removed-registration-foreign-folder') } {
        Install-Grove $package $root $links 'None' '' '0.4.2' | Out-Null
        $previous = Current-Release
        $retained = Join-Path $root ('releases/' + ('b' * 32))
        if ($Scenario -eq 'owned-stale-registration') { Copy-Item -LiteralPath $previous -Destination $retained -Recurse }
        # Match the reported three-field conflict: a previous owned version
        # and its icon/uninstall paths, with the correct product/root identity.
        $global:AppsEntry = Get-GroveUninstallRegistration $retained $root '0.4.1'
        $before = Get-GroveUninstallEntry
        $expected = Get-GroveUninstallRegistration $previous $root '0.4.2'
        if ((Get-GroveUninstallConflictMessage $before $expected) -notmatch 'DisplayIcon, DisplayVersion, UninstallString') { throw 'Fixture does not reproduce the reported conflict' }
        if ($Scenario -eq 'removed-registration-tampered') { [IO.File]::WriteAllText((Join-Path $previous 'scripts/install-windows.ps1'), 'tampered active script') }
        if ($Scenario -eq 'removed-registration-unowned') { Remove-Item -LiteralPath (Join-Path $previous '.grove-release.json') }
        if ($Scenario -eq 'removed-registration-foreign-folder') { [IO.Directory]::CreateDirectory($retained) | Out-Null }
        if ($Scenario -like 'removed-registration-*') {
            Must-Fail { Test-GroveInstall $package $root $links 'None' '' '0.4.3' }
            if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $before)) { throw 'Unverified recovery changed installation' }
            break
        }
        $events = @(Test-GroveInstall $package $root $links 'None' '' '0.4.3')
        if ($events[-1] -ne 'GROVE_PREFLIGHT_OK' -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $before) -or (Current-Release) -ne $previous) { throw 'Recovery preflight changed the installation' }
        Install-Grove $package $root $links 'None' '' '0.4.3' | Out-Null
        $current = Current-Release
        if ($current -eq $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) (Get-GroveUninstallRegistration $current $root '0.4.3'))) { throw 'Owned stale registration did not upgrade' }
        Uninstall-Grove $root $links | Out-Null
    }
    'owned-stale-registration-rollback' {
        New-Install; $previous = Current-Release
        $global:AppsEntry.DisplayVersion = '0.4.1'
        $before = Get-GroveUninstallEntry
        $pending = Join-Path $root '.grove-install.pending.json'
        [IO.File]::WriteAllText($pending, 'preserve interrupted record')
        Must-Fail { Install-Grove $package $root $links 'None' '' '0.4.3' }
        if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $before)) { throw 'Recovery rollback did not restore exact previous registration' }
        Remove-Item -LiteralPath $pending
        # A later successful retry repairs the version metadata.
        Install-Grove $package $root $links 'None' '' '0.4.3' | Out-Null
        Uninstall-Grove $root $links | Out-Null
    }
    'owned-stale-registration-tampered' {
        New-Install; $previous = Current-Release
        $global:AppsEntry.DisplayVersion = '0.4.1'
        $before = Get-GroveUninstallEntry
        [IO.File]::WriteAllText((Join-Path $previous 'scripts/install-windows.ps1'), 'foreign modified script')
        Must-Fail { Test-GroveInstall $package $root $links 'None' '' '0.4.3' }
        if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $before)) { throw 'Tampered recovery changed the installation' }
    }
    'owned-stale-registration-foreign' {
        New-Install; $previous = Current-Release
        foreach ($attack in @('command','root','product','extra-count','extra-keys','traversal')) {
            $global:AppsEntry = Get-GroveUninstallRegistration $previous $root '0.4.1'
            switch ($attack) {
                'command' { $global:AppsEntry.UninstallString = 'foreign.exe' }
                'root' { $global:AppsEntry.InstallLocation = (Join-Path $Fixture 'another-product') }
                'product' { $global:AppsEntry.GroveProduct = 'another.product' }
                'extra-count' { $global:AppsEntry['Count'] = 10 }
                'extra-keys' { $global:AppsEntry['Keys'] = 'foreign-value' }
                'traversal' { $global:AppsEntry.DisplayIcon = (Join-Path $root 'releases/../foreign.exe') }
            }
            $before = Get-GroveUninstallEntry
            Must-Fail { Test-GroveInstall $package $root $links 'None' '' '0.4.3' }
            if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $before)) { throw 'Foreign registration was changed' }
        }
        $global:AppsEntry = Get-GroveUninstallRegistration $previous $root '0.1.0-beta.1'
        Uninstall-Grove $root $links | Out-Null
    }
    'owned-stale-registration-manifest' {
        New-Install; $previous = Current-Release
        $global:AppsEntry.DisplayVersion = '0.4.1'
        $before = Get-GroveUninstallEntry
        $record = Join-Path $previous '.grove-release.json'
        $original = [IO.File]::ReadAllText($record)
        foreach ($attack in @('traversal','rooted','alias','self','missing-script','product')) {
            $manifest = $original | ConvertFrom-Json
            switch ($attack) {
                'traversal' { $manifest.Files += '../foreign.txt' }
                'rooted' { $manifest.Files += (Join-Path $Fixture 'foreign.txt') }
                'alias' { $manifest.Files += 'scripts/./install-windows.ps1' }
                'self' { $manifest.Files += '.grove-release.json' }
                'missing-script' { $manifest.Files = @($manifest.Files | Where-Object { $_ -ne 'scripts/install-windows.ps1' }) }
                'product' { $manifest.Product = 'another.product' }
            }
            Write-GroveJson $record $manifest
            Must-Fail { Test-GroveInstall $package $root $links 'None' '' '0.4.3' }
            if ((Current-Release) -ne $previous -or !(Test-GroveUninstallEntryMatch (Get-GroveUninstallEntry) $before)) { throw 'Invalid ownership manifest changed the installation' }
        }
        [IO.File]::WriteAllText($record, $original)
        $global:AppsEntry.DisplayVersion = '0.1.0-beta.1'
        Uninstall-Grove $root $links | Out-Null
    }
    'app-entry-migration' {
        New-Install; $global:AppsEntry = $null
        Install-Grove $package $root $links 'None' '' | Out-Null
        if ($global:AppsEntry.DisplayName -ne 'Grove') { throw 'Previous preview did not gain Windows uninstall entry' }
        Uninstall-Grove $root $links | Out-Null
    }
    'app-entry-new-race' {
        $global:RaceNewAppEntry = $true; Must-Fail { New-Install }
        if ((Test-Path -LiteralPath $root) -or $global:AppsEntry.Foreign -ne 'preserve') { throw 'Concurrent foreign entry removed' }
    }
    'protocol' {
        $events = @(Install-Grove $package $root $links 'None' '')
        $expectedResult = 'GROVE_RESULT|' + (Join-Path (Current-Release) 'grove-music.exe')
        if ($events[-1] -ne $expectedResult -or @($events | Where-Object { $_ -like 'GROVE_RESULT|*' }).Count -ne 1) { throw 'Missing/duplicate exact installed result' }
        $previousPercent = -1
        foreach ($event in $events | Where-Object { $_ -like 'GROVE_PROGRESS|*' }) {
            if ($event -notmatch '^GROVE_PROGRESS\|([0-9]+)\|[^|\r\n]+$') { throw 'Malformed progress event' }
            $percent = [int]$Matches[1]
            if ($percent -lt $previousPercent -or $percent -gt 100) { throw 'Invalid progress ordering' }
            $previousPercent = $percent
        }
        if ($previousPercent -ne 100 -or $global:Registry.Count) { throw 'Default setup required manual browser registration' }
        Uninstall-Grove $root $links | Out-Null
    }
    default { throw 'Unknown scenario' }
}
if ([IO.File]::ReadAllText($session) -ne 'synthetic unchanged data') { throw 'Saved session fixture changed' }
'PASS isolated installer lifecycle'
