param(
    [Parameter(Mandatory)][string]$Fxc,
    [string]$Output = (Join-Path $PSScriptRoot '../../crates/adv-dx/tests/fixtures/dxbc')
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $Output | Out-Null
$source = Join-Path $PSScriptRoot 'dxbc.hlsl'

function Compile($Entry, $Profile) {
    $define = @()
    if ($Profile -match 'level_9') { $define = @('/D', 'LEVEL9=1') }
    & $Fxc /nologo /T $Profile /E $Entry @define /O3 /Qstrip_debug /Qstrip_reflect /Qstrip_priv /Fo (Join-Path $Output "$Profile.dxbc") $source
    if ($LASTEXITCODE -ne 0) { throw "FXC failed for $Profile" }
}

foreach ($model in @('4_0', '4_1', '5_0')) {
    Compile vs "vs_$model"
    Compile indexed_ps "ps_$model"
    Compile gs "gs_$model"
    Compile cs "cs_$model"
}
Compile hs hs_5_0
Compile ds ds_5_0
foreach ($level in @('9_1', '9_3')) {
    Compile vs "vs_4_0_level_$level"
    Compile ps "ps_4_0_level_$level"
}
