param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('egui', 'gpui', 'all')]
    [string]$Renderer,

    [Parameter(Mandatory = $true, Position = 1)]
    [string]$Document,

    [ValidateRange(2, 300)]
    [int]$Seconds = 5,

    [switch]$AutoScroll
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$documentPath = (Resolve-Path -LiteralPath $Document).Path
$targetDirectory = Join-Path $repoRoot 'target\renderer-spikes'
$env:CARGO_TARGET_DIR = $targetDirectory
$env:MARKDOWN_SPIKE_SECONDS = $Seconds.ToString()

if ($AutoScroll) {
    $env:MARKDOWN_SPIKE_AUTOSCROLL = '1'
} else {
    Remove-Item Env:MARKDOWN_SPIKE_AUTOSCROLL -ErrorAction SilentlyContinue
}

function Invoke-RendererSpike {
    param(
        [string]$Name,
        [string]$Manifest,
        [string]$Executable
    )

    if ($Name -eq 'gpui' -and $AutoScroll) {
        Write-Warning 'GPUI TextView does not expose its private list handle; running the GPUI pass idle.'
        Remove-Item Env:MARKDOWN_SPIKE_AUTOSCROLL -ErrorAction SilentlyContinue
    }

    cargo build --release --manifest-path $Manifest
    if ($LASTEXITCODE -ne 0) {
        throw "$Name release build failed"
    }

    $binary = Get-Item -LiteralPath $Executable
    Write-Output "MARKDOWN_SPIKE_BUILD renderer=$Name binary_mib=$([math]::Round($binary.Length / 1MB, 2))"
    & $Executable $documentPath
    if ($LASTEXITCODE -ne 0) {
        throw "$Name benchmark failed with exit code $LASTEXITCODE"
    }
}

if ($Renderer -in @('egui', 'all')) {
    Invoke-RendererSpike `
        -Name 'egui-extended' `
        -Manifest (Join-Path $PSScriptRoot 'egui-extended\Cargo.toml') `
        -Executable (Join-Path $targetDirectory 'release\markdown-spike-egui-extended.exe')
}

if ($Renderer -in @('gpui', 'all')) {
    Invoke-RendererSpike `
        -Name 'gpui-text-view' `
        -Manifest (Join-Path $PSScriptRoot 'gpui-text-view\Cargo.toml') `
        -Executable (Join-Path $targetDirectory 'release\markdown-spike-gpui-text-view.exe')
}

