param(
    [Parameter(Mandatory)][string]$Binary,
    [string]$Output = 'tmp/rendering-measurement.json',
    [int]$Runs = 3,
    [int]$Seconds = 10,
    [string]$Fixtures = 'tests/fixtures/rendering',
    [switch]$Poc,
    [switch]$Passive,
    [string[]]$Scenarios = @('empty','ordinary','mermaid','large','reopen')
)
$ErrorActionPreference = 'Stop'
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$fixturePath = (Resolve-Path -LiteralPath $Fixtures).Path
if (-not ('ReaderMetrics' -as [type])) {
    Add-Type @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
public static class ReaderMetrics {
    [StructLayout(LayoutKind.Sequential)]
    public struct Counters {
        public uint cb, faults;
        public UIntPtr peakWS, ws, peakPaged, paged, peakNonPaged, nonPaged, pagefile, peakPagefile, privateBytes, privateWS, sharedCommit;
    }
    [DllImport("psapi.dll", SetLastError=true)]
    static extern bool GetProcessMemoryInfo(IntPtr process, ref Counters counters, uint size);
    public static Counters Read(Process p) {
        var c = new Counters(); c.cb = (uint)Marshal.SizeOf<Counters>();
        if (!GetProcessMemoryInfo(p.Handle, ref c, c.cb)) throw new System.ComponentModel.Win32Exception();
        return c;
    }
}
'@
}
$results = [Collections.Generic.List[object]]::new()
foreach ($run in 1..$Runs) {
    foreach ($scenario in $Scenarios) {
        if ($Poc -and $scenario -in @('reopen','repeat-same')) { Write-Warning 'PoC does not implement document switching; skipping'; continue }
        if ($Passive -and $scenario -in @('reopen','repeat-same')) { Write-Warning 'Passive sampling cannot drive document switching; skipping'; continue }
        $info = [Diagnostics.ProcessStartInfo]::new($binaryPath)
        $info.UseShellExecute = $false
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        if ($scenario -ne 'empty') {
            $name = if ($scenario -in @('reopen','repeat-same')) { 'ordinary' } else { $scenario }
            $info.ArgumentList.Add((Join-Path $fixturePath "$name.md"))
        }
        $null = $info.Environment.Remove('NATIVE_MARKDOWN_RENDERER')
        # The benchmark drives document replacement in the actual UI event loop.
        $info.Environment['NATIVE_MARKDOWN_BENCHMARK'] = $(if ($scenario -in @('reopen','repeat-same')) {'reopen'} else {'idle'})
        $info.Environment['NATIVE_MARKDOWN_BENCHMARK_SECONDS'] = ($Seconds + 15).ToString()
        $info.Environment['NATIVE_MARKDOWN_BENCHMARK_WARMUP_MS'] = '1500'
        $info.Environment['NATIVE_MARKDOWN_BENCHMARK_STEP_MS'] = '1000'
        $null = $info.Environment.Remove('NATIVE_MARKDOWN_BENCHMARK_STEPS')
        if ($scenario -eq 'reopen' -and -not $Poc) {
            $info.Environment['NATIVE_MARKDOWN_BENCHMARK'] = 'switch-idle'
            $info.Environment['NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT'] = Join-Path $fixturePath 'mermaid.md'
        }
        # PoC and final reader share a profile chosen per measurement batch, not other apps.
        $info.Environment['NATIVE_MARKDOWN_WEB_PROFILE'] = [IO.Path]::GetFullPath('tmp/webview-measure-profile')
        $info.Environment['NATIVE_MARKDOWN_MEASURE_SCENARIO'] = $scenario
        if ($Passive) { $null = $info.Environment.Remove('NATIVE_MARKDOWN_BENCHMARK') }
        $p = [Diagnostics.Process]::new(); $p.StartInfo = $info
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $null = $p.Start()
        $stdout = $p.StandardOutput.ReadToEndAsync(); $stderr = $p.StandardError.ReadToEndAsync()
        $known = [Collections.Generic.HashSet[int]]::new(); $null = $known.Add($p.Id)
        $samples = [Collections.Generic.List[object]]::new()
        $windowMs = $null
        try {
            while ($clock.Elapsed.TotalSeconds -lt $Seconds -and -not $p.HasExited) {
                $p.Refresh()
                if ($null -eq $windowMs -and $p.MainWindowHandle -ne 0) { $windowMs = $clock.Elapsed.TotalMilliseconds }
                $inventory = @(Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CommandLine,ExecutablePath)
                do {
                    $added = $false
                    foreach ($item in $inventory) {
                        if ($known.Contains([int]$item.ParentProcessId) -and $known.Add([int]$item.ProcessId)) { $added = $true }
                    }
                } while ($added)
                $rows = @(
                    foreach ($item in $inventory) {
                        if (-not $known.Contains([int]$item.ProcessId)) { continue }
                        try {
                            $proc = Get-Process -Id $item.ProcessId -ErrorAction Stop
                            $c = [ReaderMetrics]::Read($proc)
                            [pscustomobject]@{pid=$proc.Id; parent=$item.ParentProcessId; name=$item.Name; command=$item.CommandLine;path=$item.ExecutablePath;
                                ws=[uint64]$c.ws; privateWS=[uint64]$c.privateWS; privateBytes=[uint64]$c.privateBytes; cpuSeconds=$proc.TotalProcessorTime.TotalSeconds}
                        } catch { } # A process can exit between inventory and sampling.
                    }
                )
                $samples.Add([pscustomobject]@{ms=$clock.Elapsed.TotalMilliseconds; processes=$rows})
                Start-Sleep -Milliseconds 250
            }
        } finally {
            if (-not $p.HasExited) { $p.Kill($true) }
            $p.WaitForExit()
        }
        $results.Add([pscustomobject]@{run=$run; scenario=$scenario; renderer='webview'; windowVisibleMs=$windowMs; elapsedMs=$clock.Elapsed.TotalMilliseconds;
            stdout=$stdout.GetAwaiter().GetResult(); stderr=$stderr.GetAwaiter().GetResult(); samples=$samples.ToArray()})
        Write-Host "run=$run scenario=$scenario samples=$($samples.Count)"
    }
}
$outputPath = [IO.Path]::GetFullPath($Output)
$null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $outputPath)
[pscustomobject]@{date=[DateTime]::UtcNow.ToString('o'); binary=$binaryPath; binarySHA256=(Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash; os=[Environment]::OSVersion.VersionString;
    logicalCPUs=[Environment]::ProcessorCount; runtimeAlreadyInstalled=$true;
    note='WS includes shared pages: sum is NOT exclusive memory. PrivateWS is resident private memory; PrivateBytes is private commit. First window handle is NOT first readable paint. No OS cache flush.';
    results=$results.ToArray()} | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $outputPath -Encoding utf8
Write-Host $outputPath
