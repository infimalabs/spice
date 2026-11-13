# First-logon configuration for the Spice Windows 11 ARM developer guest.
#
# Windows Setup runs this once from the provisioning media. It installs the
# pinned virtio network driver, OpenSSH server, and x64 VC++ runtime, admits
# only the host key written beside it, installs the task that announces SSH
# readiness to the host over the serial port, and powers the guest off so the
# host can seal the installed disk as the recorded clean base.
param([Parameter(Mandatory = $true)][string]$Media)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$payload = Join-Path $Media 'spice'

New-Item -ItemType Directory -Force -Path 'C:\spice\runner', 'C:\spice\work', 'C:\spice\tmp' | Out-Null
Start-Transcript -Path 'C:\spice\provision.log' | Out-Null
try {
    foreach ($driver in 'drivers\network\netkvm.inf', 'drivers\serial\vioser.inf') {
        & pnputil.exe /add-driver (Join-Path $payload $driver) /install
        if ($LASTEXITCODE -notin 0, 3010) { throw "pnputil exited $LASTEXITCODE for $driver" }
    }

    $msi = Start-Process -FilePath 'msiexec.exe' -Wait -PassThru -ArgumentList @(
        '/i', ('"{0}"' -f (Join-Path $payload 'openssh.msi')), '/qn', '/norestart')
    if ($msi.ExitCode -notin 0, 3010) { throw "OpenSSH installer exited $($msi.ExitCode)" }

    # Rust MSVC executables load VCRUNTIME140.dll; Windows does not ship it.
    $runtime = Start-Process -FilePath (Join-Path $payload 'vc_redist.x64.exe') -Wait -PassThru `
        -ArgumentList '/install', '/quiet', '/norestart'
    if ($runtime.ExitCode -notin 0, 3010) { throw "x64 VC++ runtime installer exited $($runtime.ExitCode)" }

    $ssh = 'C:\ProgramData\ssh'
    New-Item -ItemType Directory -Force -Path $ssh | Out-Null
    Set-Content -Encoding ascii -Path (Join-Path $ssh 'sshd_config') -Value @(
        'PubkeyAuthentication yes',
        'PasswordAuthentication no',
        'KbdInteractiveAuthentication no',
        'Subsystem sftp sftp-server.exe',
        'Match Group administrators',
        '    AuthorizedKeysFile __PROGRAMDATA__/ssh/administrators_authorized_keys')
    $keys = Join-Path $ssh 'administrators_authorized_keys'
    Copy-Item -Force -Path (Join-Path $payload 'authorized_keys') -Destination $keys
    & icacls.exe $keys /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F'
    if ($LASTEXITCODE -ne 0) { throw "icacls exited $LASTEXITCODE" }

    Set-Service -Name sshd -StartupType Automatic
    Start-Service -Name sshd
    New-NetFirewallRule -Name 'spice-guest-sshd' -DisplayName 'Spice guest SSH' `
        -Direction Inbound -Protocol TCP -LocalPort 22 -Action Allow -Profile Any | Out-Null

    # The host learns that sshd listens from one line on the guest's
    # org.spice.ready virtio-serial port. sshd reports Running to the service
    # manager before it listens, so the announcer runs when sshd logs an event
    # (its "Server listening" line among them) and at startup, and writes only
    # once port 22 is listening.
    $announcer = 'C:\spice\runner\announce.ps1'
    Set-Content -Encoding ascii -Path $announcer -Value @(
        '$ErrorActionPreference = ''Stop''',
        'if (-not (Get-NetTCPConnection -State Listen -LocalPort 22 -ErrorAction SilentlyContinue)) { exit 0 }',
        '# .NET streams refuse device paths; cmd redirection opens the port with CreateFile.',
        '& cmd.exe /c ''echo ready> \\.\Global\org.spice.ready''',
        'if ($LASTEXITCODE -ne 0) { throw "announcing on org.spice.ready exited $LASTEXITCODE" }')
    $eventClass = Get-CimClass -ClassName MSFT_TaskEventTrigger -Namespace Root/Microsoft/Windows/TaskScheduler
    $onSshdEvent = New-CimInstance -CimClass $eventClass -ClientOnly
    $onSshdEvent.Enabled = $true
    $onSshdEvent.Subscription = '<QueryList><Query Id="0" Path="OpenSSH/Operational">' +
        '<Select Path="OpenSSH/Operational">*[System[Provider[@Name=''OpenSSH'']]]</Select></Query></QueryList>'
    $atStartup = New-ScheduledTaskTrigger -AtStartup
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' `
        -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File $announcer"
    $principal = New-ScheduledTaskPrincipal -UserId 'S-1-5-18' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -MultipleInstances IgnoreNew `
        -ExecutionTimeLimit (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName 'spice-ready' -Trigger @($onSshdEvent, $atStartup) -Action $action `
        -Principal $principal -Settings $settings -Force | Out-Null

    & powercfg.exe /hibernate off
    & powercfg.exe /change standby-timeout-ac 0
    & powercfg.exe /change monitor-timeout-ac 0
} catch {
    # Record the failure before the transcript closes so C:\spice\provision.log
    # explains a guest that powers off without reachable SSH.
    Write-Output "setup failed: $_"
    throw
} finally {
    Stop-Transcript | Out-Null
    Stop-Computer -Force
}
