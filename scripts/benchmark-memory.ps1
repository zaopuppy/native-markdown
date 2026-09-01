param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Document,

    [ValidateSet('idle', 'view-modes', 'zoom', 'zoom-source', 'zoom-split', 'reopen', 'scroll', 'image-release', 'all')]
    [string]$Scenario = 'all',

    [string]$SecondaryDocument,

    [ValidateSet('debug', 'release')]
    [string]$Profile = 'release',

    [ValidateRange(1, 300)]
    [int]$Seconds = 5,

    [ValidateRange(0, 30000)]
    [int]$WarmupMs = 1500,

    [ValidateRange(10, 10000)]
    [int]$StepMs = 100,

    [ValidateRange(0, 1000000)]
    [int]$Steps = 0,

    [ValidateRange(1, 1000000)]
    [int]$SwitchStep = 100,

    [ValidateRange(1, 65536)]
    [int]$MaxPrivateWorkingSetMiB = 160,

    [ValidateRange(1, 65536)]
    [int]$MaxPrivateBytesMiB = 160,

    [ValidateRange(0, 65536)]
    [int]$MaxGrowthMiB = 80,

    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$documentPath = (Resolve-Path -LiteralPath $Document).Path
$secondaryDocumentPath = if ($SecondaryDocument) {
    (Resolve-Path -LiteralPath $SecondaryDocument).Path
} else {
    $null
}
$profileArguments = @()
if ($Profile -eq 'release') {
    $profileArguments += '--release'
}
$binary = Join-Path $repoRoot "target\$Profile\native-markdown.exe"
$scenarios = if ($Scenario -eq 'all') {
    @('idle', 'view-modes', 'zoom', 'zoom-source', 'zoom-split', 'reopen', 'scroll')
} else {
    @($Scenario)
}

if (-not $SkipBuild) {
    Push-Location $repoRoot
    try {
        & cargo build @profileArguments
        if ($LASTEXITCODE -ne 0) {
            throw "native-markdown $Profile build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "benchmark binary does not exist: $binary"
}

if ($Scenario -eq 'image-release' -and -not $secondaryDocumentPath) {
    throw '-SecondaryDocument is required for the image-release scenario'
}

if ('scroll' -in $scenarios -or 'image-release' -in $scenarios) {
    if ($env:OS -ne 'Windows_NT') {
        throw 'the scroll scenario currently requires Windows'
    }
    if (-not ('NativeMarkdown.BenchmarkInput' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace NativeMarkdown {
    public static class BenchmarkInput {
        [StructLayout(LayoutKind.Sequential)]
        private struct Rect {
            public int Left;
            public int Top;
            public int Right;
            public int Bottom;
        }

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool PostMessage(IntPtr hWnd, uint message, IntPtr wParam, IntPtr lParam);

        [DllImport("user32.dll", SetLastError = true)]
        private static extern bool GetWindowRect(IntPtr hWnd, out Rect rect);

        public static bool PostWheelAtCenter(IntPtr hWnd, int delta) {
            Rect rect;
            if (!GetWindowRect(hWnd, out rect)) {
                return false;
            }
            int x = rect.Left + (rect.Right - rect.Left) / 2;
            int y = rect.Top + (rect.Bottom - rect.Top) / 2;
            long wParam = ((long)(ushort)(short)delta) << 16;
            long lParam = ((long)(ushort)y << 16) | (ushort)x;
            return PostMessage(hWnd, 0x020A, new IntPtr(wParam), new IntPtr(lParam));
        }
    }
}
'@
    }
}

function Start-BenchmarkProcess {
    param([string]$BenchmarkScenario)

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $binary
    $startInfo.ArgumentList.Add($documentPath)
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $false
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK'] = $BenchmarkScenario
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_SECONDS'] = $Seconds.ToString()
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_WARMUP_MS'] = $WarmupMs.ToString()
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_STEP_MS'] = $StepMs.ToString()
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_WS_MIB'] = $MaxPrivateWorkingSetMiB.ToString()
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_MAX_PRIVATE_BYTES_MIB'] = $MaxPrivateBytesMiB.ToString()
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_MAX_GROWTH_MIB'] = $MaxGrowthMiB.ToString()
    $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_SWITCH_STEP'] = $SwitchStep.ToString()
    if ($secondaryDocumentPath) {
        $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_SECONDARY_DOCUMENT'] = $secondaryDocumentPath
    }
    if ($Steps -gt 0) {
        $startInfo.Environment['NATIVE_MARKDOWN_BENCHMARK_STEPS'] = $Steps.ToString()
    } else {
        $null = $startInfo.Environment.Remove('NATIVE_MARKDOWN_BENCHMARK_STEPS')
    }

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw "failed to start benchmark scenario $BenchmarkScenario"
    }
    return $process
}

function Send-ScrollUntilExit {
    param(
        [Diagnostics.Process]$Process,
        [switch]$OnlyDown
    )

    $windowDeadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 50
        $Process.Refresh()
    } while (-not $Process.HasExited -and $Process.MainWindowHandle -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $windowDeadline)

    if ($Process.HasExited) {
        return
    }
    if ($Process.MainWindowHandle -eq [IntPtr]::Zero) {
        throw 'scroll benchmark did not expose a main window within 10 seconds'
    }

    Start-Sleep -Milliseconds ($WarmupMs + 100)
    $index = 0
    $accepted = 0
    while (-not $Process.HasExited) {
        $delta = if ($OnlyDown -or ([math]::Floor($index / 30) % 2) -eq 0) { -120 } else { 120 }
        if ([NativeMarkdown.BenchmarkInput]::PostWheelAtCenter(
            $Process.MainWindowHandle,
            $delta
        )) {
            $accepted++
        }
        $index++
        Start-Sleep -Milliseconds ([math]::Max(10, $StepMs))
        $Process.Refresh()
    }
    return [pscustomobject]@{ Sent = $index; Accepted = $accepted }
}

$results = @()
foreach ($benchmarkScenario in $scenarios) {
    Write-Output "NATIVE_MARKDOWN_BENCHMARK_RUN scenario=$benchmarkScenario profile=$Profile"
    $process = Start-BenchmarkProcess -BenchmarkScenario $benchmarkScenario
    try {
        if ($benchmarkScenario -eq 'scroll' -or $benchmarkScenario -eq 'image-release') {
            $input = Send-ScrollUntilExit -Process $process -OnlyDown:($benchmarkScenario -eq 'image-release')
            Write-Output "NATIVE_MARKDOWN_BENCHMARK_INPUT scenario=$benchmarkScenario sent=$($input.Sent) accepted=$($input.Accepted)"
        }

        $timeoutMs = $WarmupMs + ($Seconds * 1000) + 15000
        if (-not $process.WaitForExit($timeoutMs)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "benchmark scenario $benchmarkScenario exceeded its ${timeoutMs}ms watchdog"
        }

        $stdout = $process.StandardOutput.ReadToEnd()
        $stderr = $process.StandardError.ReadToEnd()
        if ($stdout) {
            Write-Output $stdout.TrimEnd()
        }
        if ($stderr) {
            Write-Error $stderr.TrimEnd()
        }

        $finished = $stdout -split "`r?`n" |
            Where-Object { $_ -like 'NATIVE_MARKDOWN_BENCHMARK event=finished*' } |
            Select-Object -Last 1
        $results += [pscustomobject]@{
            Scenario = $benchmarkScenario
            ExitCode = $process.ExitCode
            Result = if ($process.ExitCode -eq 0) { 'pass' } elseif ($process.ExitCode -eq 1) { 'fail' } else { 'error' }
            Finished = $finished
        }
    } finally {
        if (-not $process.HasExited) {
            $process.Kill($true)
            $process.WaitForExit()
        }
        $process.Dispose()
    }
}

$results | Select-Object Scenario, Result, ExitCode | Format-Table -AutoSize

$errors = @($results | Where-Object { $_.Result -eq 'error' })
if ($errors.Count -gt 0) {
    throw "memory benchmark harness failed for: $($errors.Scenario -join ', ')"
}
$failures = @($results | Where-Object { $_.Result -eq 'fail' })
if ($failures.Count -gt 0) {
    throw "memory budget failed for: $($failures.Scenario -join ', ')"
}
