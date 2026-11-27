param(
    [string]$Executable,
    [string]$ExperimentName,
    [int]$DurationSeconds = 300
)

$ErrorActionPreference = 'Stop'

$root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$exe = $Executable
$gameDir = Join-Path $root 'GTA6.Xbox.v1.0.2.1-CYBERLEEK'
$game = Join-Path $gameDir 'game_scarlett_master.exe'

# Common environment
$env:VICTORY_CONFIG = Join-Path $root 'auditoria\victory-8gb-local.toml'
$env:VICTORY_DXIL = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\dxil.dll'
$env:VICTORY_DISPLAY_MODE = '1280x720'
$env:VICTORY_LOG = 'info'
$env:VICTORY_PIPELINE_SMALL_FIRST = '1'
$env:VICTORY_BAKE_TEXTURES_MS = '0'
$env:VICTORY_TEXTURE_DROP_MIPS = '1'
$env:VICTORY_SMALL_TEXTURE_KB = '0'
$env:VICTORY_QUEUE2 = '1'
$env:VICTORY_INDIRECT_WINDOW = '1'
$env:VICTORY_ASYNC_SUBMIT = '1'
$env:VICTORY_ASYNC_SUBMIT_SAFE = '1'
$env:VICTORY_CARD_HASH_WIDE = '1'
$env:VICTORY_LATE_WAIT = '2'
$env:VICTORY_BACK_SETTLE = '0'
$env:VICTORY_MAPPING_RANGES = '1'
$env:VICTORY_HEAP_PASS_NO_SELF_MARK = '1'
$env:VICTORY_PROFILE_BUFFERS = '1'
$env:VICTORY_COPY_PLAIN = '1'
$env:VICTORY_WRITE_PLAIN = '1'

# Clear CGBJ flags
[Environment]::SetEnvironmentVariable('VICTORY_BIND_CACHE', $null, 'Process')
[Environment]::SetEnvironmentVariable('VICTORY_INDIRECT_RECOVER', $null, 'Process')
[Environment]::SetEnvironmentVariable('VICTORY_PIPELINE_WORKERS', $null, 'Process')
[Environment]::SetEnvironmentVariable('VICTORY_PIPELINE_LOWEST', $null, 'Process')

if (!(Test-Path -LiteralPath $exe -PathType Leaf) -or !(Test-Path -LiteralPath $game -PathType Leaf)) {
    throw "Executavel do VIctory ou do jogo nao encontrado: $exe"
}

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class VictoryWindow
{
    [StructLayout(LayoutKind.Sequential)]
    private struct Rect { public int Left, Top, Right, Bottom; }

    [DllImport("user32.dll")]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr window, out Rect rect);

    [DllImport("user32.dll")]
    private static extern bool GetClientRect(IntPtr window, out Rect rect);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y,
        int width, int height, uint flags);

    public static bool SetClientSize(IntPtr window, int width, int height)
    {
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            Rect outer, client;
            if (!GetWindowRect(window, out outer) || !GetClientRect(window, out client))
                return false;
            var borderWidth = (outer.Right - outer.Left) - (client.Right - client.Left);
            var borderHeight = (outer.Bottom - outer.Top) - (client.Bottom - client.Top);
            if (!SetWindowPos(window, IntPtr.Zero, 0, 0, width + borderWidth,
                height + borderHeight, 0x0002 | 0x0004 | 0x0010))
                return false;
            return GetClientRect(window, out client)
                && client.Right - client.Left == width
                && client.Bottom - client.Top == height;
        } finally {
            if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous);
        }
    }
}
"@

$runId = Get-Date -Format 'yyyyMMdd-HHmmss'
$logs = Join-Path $root "victory-diagnostics\launch-benchmark-$ExperimentName-$runId"
New-Item -ItemType Directory -Path $logs | Out-Null

$sha256 = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash
Write-Host ("Starting {0}: {1}" -f $ExperimentName, $exe)
Write-Host ("SHA256: {0}" -f $sha256)
Write-Host ("Logs: {0}" -f $logs)
Write-Host ("Duration: {0} seconds" -f $DurationSeconds)

$process = Start-Process -FilePath $exe -WorkingDirectory $gameDir -PassThru -WindowStyle Hidden `
    -ArgumentList @('run', ('"' + $game + '"'), '--execute', '--console', 'series-s', '--unimplemented', 'trap') `
    -RedirectStandardOutput (Join-Path $logs 'stdout.txt') `
    -RedirectStandardError (Join-Path $logs 'stderr.txt')

$metadata = @{
    experiment = $ExperimentName
    binary = $exe
    sha256 = $sha256
    process_id = $process.Id
    mapping_ranges = '1'
    heap_pass_no_self_mark = '1'
    bake_textures_ms = '0'
    profile_buffers = '1'
    duration_seconds = $DurationSeconds
    usage_interval_seconds = 5
}
$metadata | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $logs 'experiment.json')

$sampler = Join-Path $PSScriptRoot 'Measure-LocalUsage.ps1'
$shell = (Get-Process -Id $PID).Path
Start-Process -FilePath $shell -WindowStyle Hidden -ArgumentList @(
    '-NoProfile', '-File', ('"' + $sampler + '"'),
    '-TargetProcessId', $process.Id, '-OutputPath',
    ('"' + (Join-Path $logs 'usage.jsonl') + '"'), '-IntervalSeconds', '5'
) -RedirectStandardError (Join-Path $logs 'usage-errors.txt') | Out-Null

$deadline = [DateTime]::UtcNow.AddSeconds($DurationSeconds)
$windowConfirmed = $false

do {
    $process.Refresh()
    if ($process.HasExited) {
        throw "O VIctory encerrou antes do tempo. Logs em $logs"
    }
    if (-not $windowConfirmed -and $process.MainWindowHandle -ne [IntPtr]::Zero) {
        if ([VictoryWindow]::SetClientSize($process.MainWindowHandle, 1280, 720)) {
            Write-Host ("Area do jogo confirmada em 1280x720. Logs em {0}" -f $logs)
            $windowConfirmed = $true
        }
    }
    Start-Sleep -Milliseconds 500
} while ([DateTime]::UtcNow -lt $deadline)

if (-not $windowConfirmed) {
    throw "Nao foi possivel confirmar uma janela de 1280x720. Logs em $logs"
}

Write-Host ("Benchmark {0} concluido. Logs em {1}" -f $ExperimentName, $logs)
exit 0
