$ErrorActionPreference = 'Stop'

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$exe = Join-Path $PSScriptRoot 'target\release\victory-exp-made-mutex-fix.exe'
Get-ChildItem Env:VICTORY_* | ForEach-Object { [Environment]::SetEnvironmentVariable($_.Name, $null, 'Process') }
$env:VICTORY_MAPPING_RANGES = '1'
$env:VICTORY_HEAP_PASS_NO_SELF_MARK = '1'
$env:VICTORY_BIND_CACHE = '1'
$env:VICTORY_INDIRECT_RECOVER = '1'
$env:VICTORY_PIPELINE_WORKERS = '4'
$env:VICTORY_PIPELINE_LOWEST = '1'
$env:VICTORY_PIPELINE_SMALL_FIRST = '1'
$env:VICTORY_TEXTURE_DROP_MIPS = '1'
$env:VICTORY_SMALL_TEXTURE_KB = '0'
$env:VICTORY_QUEUE2 = '1'
$env:VICTORY_INDIRECT_WINDOW = '1'
$env:VICTORY_ASYNC_SUBMIT = '1'
$env:VICTORY_ASYNC_SUBMIT_SAFE = '1'
$env:VICTORY_CARD_HASH_WIDE = '1'
$env:VICTORY_LATE_WAIT = '2'
$env:VICTORY_BACK_SETTLE = '0'
$env:VICTORY_LOG = 'info'
$env:VICTORY_PROFILE_BUFFERS = '1'

$gameDir = Join-Path $root 'GTA6.Xbox.v1.0.2.1-CYBERLEEK'
$game = Join-Path $gameDir 'game_scarlett_master.exe'
$env:VICTORY_CONFIG = Join-Path $root 'auditoria\victory-8gb-local.toml'
$env:VICTORY_DXIL = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\dxil.dll'
$env:VICTORY_DISPLAY_MODE = '1280x720'

if (!(Test-Path -LiteralPath $exe -PathType Leaf) -or !(Test-Path -LiteralPath $game -PathType Leaf)) {
    throw 'Executavel do Victory ou do jogo nao encontrado.'
}

$runId = Get-Date -Format 'yyyyMMdd-HHmmss'
$logs = Join-Path $root "victory-diagnostics\launch-exp-$runId"
New-Item -ItemType Directory -Path $logs | Out-Null

Write-Host "Starting experimental build: $exe"
Write-Host "Logs: $logs"

$process = Start-Process -FilePath $exe -WorkingDirectory $gameDir -PassThru `
    -ArgumentList @('run', ('"' + $game + '"'), '--execute', '--console', 'series-s', '--unimplemented', 'trap') `
    -RedirectStandardOutput (Join-Path $logs 'stdout.txt') `
    -RedirectStandardError (Join-Path $logs 'stderr.txt')

# Wait longer for gameplay
$deadline = [DateTime]::UtcNow.AddSeconds(300)
do {
    $process.Refresh()
    if ($process.HasExited) {
        $exitCode = $process.ExitCode
        Write-Host "Process exited with code: $exitCode. Logs in $logs"
        break
    }
    if ($process.MainWindowHandle -ne [IntPtr]::Zero) {
        Write-Host "Window opened! PID: $($process.Id). Waiting for gameplay..."
    }
    Start-Sleep -Seconds 5
    Write-Host "Still running... ($(($deadline - [DateTime]::UtcNow).TotalSeconds | Out-String).Trim()) seconds left"
} while ([DateTime]::UtcNow -lt $deadline)

if (!$process.HasExited) {
    Write-Host "Timeout reached. Process still running. Logs in $logs"
}
