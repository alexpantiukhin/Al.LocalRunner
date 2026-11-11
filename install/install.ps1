$LocalRunnerReleasesUrl = 'https://github.com/alexpantiukhin/Al.LocalRunner/releases'
$LocalRunnerReleaseKey = '@RELEASE_PUBKEY@'
$LocalRunnerSigNamespace = 'al-localrunner-release'
$LocalRunnerArchive = 'al-localrunner-windows-x64.zip'
$LocalRunnerUsage = @'
Usage: install.ps1 [--version X.Y.Z] [--dir DIR] [--offline ARCHIVE]
                   [--role hub|broker|runner|all] [--allow-elevated]

Installs localrunner.exe for the current user after checking the signature
of SHA256SUMS and the SHA-256 of the archive. --offline takes a downloaded
archive with SHA256SUMS and SHA256SUMS.sig next to it.
'@

function Stop-Install {
    param([string]$Message)
    throw "install.ps1: $Message"
}

function Read-InstallArgs {
    param([string[]]$Arguments)
    $opts = @{ Version = ''; Dir = ''; Offline = ''; Role = 'all'; AllowElevated = $false; Help = $false }
    if ($null -eq $Arguments) {
        $Arguments = @()
    }
    $i = 0
    while ($i -lt $Arguments.Count) {
        $given = $Arguments[$i]
        if (-not $given.StartsWith('-')) {
            Stop-Install "unknown argument: $given (see --help)"
        }
        $name = $given.TrimStart('-').ToLowerInvariant()
        $value = ''
        if (@('version', 'dir', 'offline', 'role') -contains $name) {
            if ($i + 1 -ge $Arguments.Count) {
                Stop-Install "--$name needs a value"
            }
            $value = $Arguments[$i + 1]
            $i += 2
        } else {
            $i += 1
        }
        switch ($name) {
            'version' { $opts.Version = $value }
            'dir' { $opts.Dir = $value }
            'offline' { $opts.Offline = $value }
            'role' { $opts.Role = $value.ToLowerInvariant() }
            'allow-elevated' { $opts.AllowElevated = $true }
            'h' { $opts.Help = $true }
            'help' { $opts.Help = $true }
            default { Stop-Install "unknown argument: $given (see --help)" }
        }
    }
    if (@('hub', 'broker', 'runner', 'all') -notcontains $opts.Role) {
        Stop-Install '--role must be hub, broker, runner or all'
    }
    if ($opts.Version.StartsWith('v')) {
        $opts.Version = $opts.Version.Substring(1)
    }
    if ($opts.Version -cnotmatch '^[0-9A-Za-z.+-]*\z') {
        Stop-Install '--version must look like 1.2.3'
    }
    $opts
}

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-ReleaseTarget {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        Stop-Install 'this installer is for Windows; on Linux and macOS use install.sh'
    }
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) {
        $arch = $env:PROCESSOR_ARCHITECTURE
    }
    if ($arch -ne 'AMD64') {
        Stop-Install "no release for Windows on ${arch}: build from source"
    }
    $LocalRunnerArchive
}

function Get-ReleaseFile {
    param([string]$Url, [string]$OutFile)
    try {
        Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $OutFile
    } catch {
        Stop-Install "download failed: $Url"
    }
}

function Find-SshKeygen {
    if ($env:SystemRoot) {
        $builtin = Join-Path $env:SystemRoot 'System32\OpenSSH\ssh-keygen.exe'
        if (Test-Path -LiteralPath $builtin -PathType Leaf) {
            return $builtin
        }
    }
    $found = Get-Command ssh-keygen -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) {
        return $found.Path
    }
    Stop-Install 'ssh-keygen (OpenSSH 8.1 or newer) is needed to check the release signature'
}

function Test-ReleaseSignature {
    param([string]$Sums, [string]$Signature, [string]$Work)
    if (-not $LocalRunnerReleaseKey -or $LocalRunnerReleaseKey -like '@*@') {
        Stop-Install 'this install.ps1 carries no release key: take install.ps1 from the release page'
    }
    if (-not (Test-Path -LiteralPath $Signature -PathType Leaf)) {
        Stop-Install "signature $Signature is missing: nothing installed"
    }
    $keygen = Find-SshKeygen
    $ns = $LocalRunnerSigNamespace
    $signers = Join-Path $Work 'allowed_signers'
    [IO.File]::WriteAllText($signers, "$ns namespaces=`"$ns`" $LocalRunnerReleaseKey`n")
    $arguments = @('-Y', 'verify', '-f', ('"' + $signers + '"'), '-I', $ns, '-n', $ns, '-s', ('"' + $Signature + '"'))
    $proc = Start-Process -FilePath $keygen -ArgumentList $arguments -RedirectStandardInput $Sums `
        -RedirectStandardOutput (Join-Path $Work 'verify.out') -RedirectStandardError (Join-Path $Work 'verify.err') `
        -NoNewWindow -Wait -PassThru
    if ($proc.ExitCode -ne 0) {
        $errors = [IO.File]::ReadAllText((Join-Path $Work 'verify.err'))
        if ($errors -match '(illegal|unknown) option') {
            Stop-Install 'this ssh-keygen cannot check signatures: OpenSSH 8.1 or newer is needed'
        }
        Stop-Install 'the signature of SHA256SUMS does not match the release key: nothing installed'
    }
}

function Test-ReleaseChecksum {
    param([string]$Sums, [string]$File, [string]$Name)
    $expected = $null
    foreach ($line in [IO.File]::ReadAllLines($Sums)) {
        if ($line -match '^([0-9A-Fa-f]{64}) [ *](.+)$' -and $Matches[2] -eq $Name) {
            $expected = $Matches[1]
            break
        }
    }
    if (-not $expected) {
        Stop-Install "$Name is not listed in SHA256SUMS: nothing installed"
    }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $File).Hash
    if ($actual -ne $expected) {
        Stop-Install "SHA-256 of $Name does not match SHA256SUMS: nothing installed"
    }
}

function Invoke-LocalRunnerInstall {
    param([string]$Exe, [hashtable]$Opts)
    $installArgs = @('install', '--role', $Opts.Role)
    if ($Opts.Dir) {
        $installArgs += "--dir=$($Opts.Dir)"
    }
    if ($Opts.AllowElevated) {
        $installArgs += '--allow-elevated'
    }
    & $Exe @installArgs
    if ($LASTEXITCODE -ne 0) {
        Stop-Install 'localrunner install failed'
    }
    & $Exe 'doctor'
    if ($LASTEXITCODE -ne 0) {
        Write-Warning 'install.ps1: localrunner doctor reported problems (above); the installation is kept'
    }
}

function Install-LocalRunner {
    param([string[]]$Arguments)
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $opts = Read-InstallArgs $Arguments
    if ($opts.Help) {
        Write-Host $LocalRunnerUsage
        return
    }
    if ((Test-Elevated) -and -not $opts.AllowElevated) {
        Stop-Install 'refusing to run as administrator: install as an ordinary user, or add --allow-elevated'
    }
    $work = Join-Path ([IO.Path]::GetTempPath()) ('al-localrunner-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        if ($opts.Offline) {
            if (-not (Test-Path -LiteralPath $opts.Offline -PathType Leaf)) {
                Stop-Install "$($opts.Offline) is not a file"
            }
            $source = (Resolve-Path -LiteralPath $opts.Offline).ProviderPath
            $name = Split-Path -Leaf $source
            $from = Join-Path (Split-Path -Parent $source) 'SHA256SUMS'
            if (-not (Test-Path -LiteralPath $from -PathType Leaf)) {
                Stop-Install "SHA256SUMS must lie next to $source"
            }
            $sums = Join-Path $work 'SHA256SUMS'
            $archive = Join-Path $work $name
            Copy-Item -LiteralPath $source -Destination $archive
            Copy-Item -LiteralPath $from -Destination $sums
            if (Test-Path -LiteralPath "$from.sig" -PathType Leaf) {
                Copy-Item -LiteralPath "$from.sig" -Destination "$sums.sig"
            }
        } else {
            $name = Get-ReleaseTarget
            if ($opts.Version) {
                $base = "$LocalRunnerReleasesUrl/download/v$($opts.Version)"
            } else {
                $base = "$LocalRunnerReleasesUrl/latest/download"
            }
            [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
            $sums = Join-Path $work 'SHA256SUMS'
            $archive = Join-Path $work $name
            Get-ReleaseFile "$base/SHA256SUMS" $sums
            Get-ReleaseFile "$base/SHA256SUMS.sig" "$sums.sig"
            Get-ReleaseFile "$base/$name" $archive
        }
        Test-ReleaseSignature -Sums $sums -Signature "$sums.sig" -Work $work
        Test-ReleaseChecksum -Sums $sums -File $archive -Name $name
        $bin = Join-Path $work 'bin'
        Expand-Archive -LiteralPath $archive -DestinationPath $bin
        $exe = Join-Path $bin 'localrunner.exe'
        if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
            Stop-Install "$name has no localrunner.exe"
        }
        Invoke-LocalRunnerInstall -Exe $exe -Opts $opts
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($env:LOCALRUNNER_INSTALL_SOURCED -ne '1') {
    Install-LocalRunner -Arguments $args
} else {
    Write-Warning 'install.ps1: LOCALRUNNER_INSTALL_SOURCED=1 is set: functions loaded for tests, nothing installed'
}
