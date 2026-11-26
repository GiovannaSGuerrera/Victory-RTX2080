param(
    [int]$TargetProcessId,
    [Parameter(Mandatory=$true)][string]$OutputPath,
    [ValidateRange(1,60)][int]$IntervalSeconds = 2,
    [int]$MaxSamples = 0,
    [switch]$SelfTest
)
$ErrorActionPreference = 'Stop'
if ($SelfTest) { $TargetProcessId = $PID }
$tracked = Get-Process -Id $TargetProcessId
if (!$SelfTest -and $tracked.Path -notlike 'J:\GTA6\victory-8gb\target\release\victory*.exe') {
    throw 'O processo nao pertence ao runtime ativo.'
}
$started = $tracked.StartTime
$logical = [Environment]::ProcessorCount
$previousCpu = $tracked.TotalProcessorTime.TotalMilliseconds
$previousTime = [DateTime]::UtcNow
$previousThreads = @{}
$samples = 0
while ($true) {
    Start-Sleep -Seconds $IntervalSeconds
    try {
        $tracked = Get-Process -Id $TargetProcessId -ErrorAction Stop
        if ($tracked.StartTime -ne $started) { break }
        $now = [DateTime]::UtcNow
        $elapsed = ($now - $previousTime).TotalMilliseconds
        $cpu = $tracked.TotalProcessorTime.TotalMilliseconds
    } catch { break }
    $cost = [Diagnostics.Stopwatch]::StartNew()
    $errors = @()
    $threadsNow = @{}
    $threadUsage = @()
    foreach ($thread in $tracked.Threads) {
        try {
            $ms = $thread.TotalProcessorTime.TotalMilliseconds
            $threadsNow[$thread.Id] = $ms
            if ($previousThreads.ContainsKey($thread.Id)) {
                $threadUsage += [pscustomobject]@{
                    id = $thread.Id
                    # 100% means one fully occupied logical processor.
                    one_logical_cpu_pct = [Math]::Round(100 * ($ms - $previousThreads[$thread.Id]) / $elapsed, 2)
                }
            }
        } catch { $errors += 'thread-exited-or-unavailable' }
    }
    $processors = @()
    try {
        $processors = @(Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor |
            Select-Object Name,PercentProcessorTime)
    } catch { $errors += 'logical-cpu-counters-unavailable' }
    $gpu = $null
    try {
        # Device-wide utilization includes other applications too.
        $line = & nvidia-smi --query-gpu=utilization.gpu,utilization.memory,memory.used,memory.total,power.draw,temperature.gpu --format=csv,noheader,nounits 2>$null
        if ($LASTEXITCODE -ne 0) { throw 'nvidia-smi failed' }
        $gpu = @($line | ConvertFrom-Csv -Header gpu_pct,memory_controller_pct,vram_used_mib,vram_total_mib,power_w,temperature_c)
    } catch { $errors += 'nvidia-counters-unavailable' }
    $row = [ordered]@{
        utc = $now.ToString('o')
        process_id = $TargetProcessId
        process_path = $tracked.Path
        logical_processors = $logical
        process_cpu_total_pct = [Math]::Round(100 * ($cpu - $previousCpu) / $elapsed / $logical, 2)
        busiest_threads = @($threadUsage | Sort-Object one_logical_cpu_pct -Descending | Select-Object -First 8)
        system_logical_cpu_pct = $processors
        gpu_device_wide = $gpu
        private_bytes = $tracked.PrivateMemorySize64
        working_set_bytes = $tracked.WorkingSet64
        sample_cost_ms = $cost.ElapsedMilliseconds
        errors = $errors
    }
    $row | ConvertTo-Json -Depth 6 -Compress | Add-Content -LiteralPath $OutputPath -Encoding UTF8
    $previousCpu = $cpu
    $previousTime = $now
    $previousThreads = $threadsNow
    $samples++
    if ($MaxSamples -gt 0 -and $samples -ge $MaxSamples) { break }
}
