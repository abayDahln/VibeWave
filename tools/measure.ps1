# Measure private memory (whole process tree) and CPU of a running process.
# Usage: powershell -File tools/measure.ps1 -ProcessId <pid> [-Seconds 5]
param([int]$ProcessId, [int]$Seconds = 5)

function Get-Tree([int]$id) {
    $all = Get-CimInstance Win32_Process
    $ids = @($id)
    $queue = @($id)
    while ($queue.Count -gt 0) {
        $p = $queue[0]; $queue = $queue[1..($queue.Count)]
        $kids = $all | Where-Object { $_.ParentProcessId -eq $p } | ForEach-Object { [int]$_.ProcessId }
        $ids += $kids; $queue += $kids
    }
    $ids | Select-Object -Unique
}

$tree = Get-Tree $ProcessId
$procs = $tree | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
$cpu0 = [TimeSpan]::Zero
foreach ($p in $procs) { $cpu0 += $p.TotalProcessorTime }
$t0 = Get-Date
Start-Sleep -Seconds $Seconds
$procs = $tree | ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
$cpu1 = [TimeSpan]::Zero
foreach ($p in $procs) { $cpu1 += $p.TotalProcessorTime }
$elapsed = ((Get-Date) - $t0).TotalSeconds
$cores = [Environment]::ProcessorCount
$cpuPct = (($cpu1 - [TimeSpan]$cpu0).TotalSeconds / $elapsed / $cores) * 100
$priv = ($procs | Measure-Object -Property PrivateMemorySize64 -Sum).Sum / 1MB
$ws = ($procs | Measure-Object -Property WorkingSet64 -Sum).Sum / 1MB
"processes={0} private_mb={1:N1} working_set_mb={2:N1} cpu_pct_of_machine={3:N2}" -f $procs.Count, $priv, $ws, $cpuPct
