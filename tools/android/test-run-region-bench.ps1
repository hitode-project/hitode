<#
.SYNOPSIS
Validate headless region benchmark plans without builds, files or a device.
.DESCRIPTION
Run ./tools/android/test-run-region-bench.ps1. Fixed inputs exercise validation
and command assembly; output is an assertion count. No runtime artifacts.
#>
$ErrorActionPreference = 'Stop'
$runner = Join-Path $PSScriptRoot 'run-region-bench.ps1'
$assertions = 0
function Assert($condition) { if (-not $condition) { throw 'Benchmark plan assertion failed.' }; $script:assertions++ }
$arguments = @{ TargetDir = (Join-Path $PSScriptRoot '../../target/bench-plan'); Output = (Join-Path $PSScriptRoot '../../target/bench-plan-output'); WhatIf = $true }
$plan = & $runner @arguments -BuildOnly | ConvertFrom-Json
Assert $plan.build_only
Assert ($plan.adb_port -eq 5037)
Assert ($plan.cargo_arguments -contains 'host_region_bench')
Assert ($plan.cargo_arguments -contains '--locked')
Assert ($plan.cpu_mask -eq 'auto')
Assert ($plan.shell.Contains('ADV_SAFEPOINT=off'))
$poll = & $runner @arguments -Safepoint poll -LockOwner spt | ConvertFrom-Json
Assert ($poll.shell.Contains('ADV_SAFEPOINT=poll'))
$plan = & $runner @arguments -SkipBuild -CpuMask 40 -Corpus 'local-corpus' | ConvertFrom-Json
Assert $plan.skip_build
Assert ($plan.shell.Contains('taskset 40 timeout 300'))
Assert ($plan.shell.Contains('ADV_GIR_LIFT_CORPUS='))
Assert ($plan.shell.Contains('unset ADV_JIT_VERIFY ADV_DIRECT_MEMORY ADV_HOST_FEATURES'))
Assert ($plan.shell.Contains('ADV_PERF_MAP_DIR;'))
Assert ($plan.cooldown_seconds -eq 60 -and $plan.cleanup)
$rejected = $false
try { & $runner @arguments -BuildOnly -SkipBuild | Out-Null } catch { $rejected = $true }
Assert $rejected
$rejected = $false
try { & $runner @arguments -CpuMask '80;exit' | Out-Null } catch { $rejected = $true }
Assert $rejected
. $runner @arguments | Out-Null
$choices = @(Get-CpuChoices @('Cpus_allowed_list: 0-2,4', '0 800', '1 900', '2 900', '4 1800', '7 2400'))
Assert (($choices.core -join ',') -eq '4,2,1,0')
$rejected = $false
try { Get-CpuChoices @('0 800') | Out-Null } catch { $rejected = $true }
Assert $rejected
Write-Output "Region benchmark plan checks: $assertions passed."
