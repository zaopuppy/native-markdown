param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('egui', 'gpui')]
    [string]$Renderer,

    [Parameter(Mandatory = $true, Position = 1)]
    [string]$Document
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$documentPath = (Resolve-Path -LiteralPath $Document).Path
$targetDirectory = Join-Path $repoRoot 'target\renderer-spikes'

$manifest = switch ($Renderer) {
    'egui' { Join-Path $PSScriptRoot 'egui-extended\Cargo.toml' }
    'gpui' { Join-Path $PSScriptRoot 'gpui-text-view\Cargo.toml' }
}

$env:CARGO_TARGET_DIR = $targetDirectory
cargo run --release --manifest-path $manifest -- $documentPath

