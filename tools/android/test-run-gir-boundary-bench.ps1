<#
.SYNOPSIS
Test boundary benchmark plans, parsers and atomic ownership without a device.
.DESCRIPTION
Usage: ./tools/android/test-run-gir-boundary-bench.ps1 after setting task TEMP/TMP.
Inputs: runner and task-local TEMP. Output: assertion count; a tiny temporary lock
under TEMP is removed in finally. No build, device, APK or shared-lock access.
#>
$ErrorActionPreference = 'Stop'
$runner = Join-Path $PSScriptRoot 'run-gir-boundary-bench.ps1'
$assertions = 0
function Assert($Condition) { if (-not $Condition) { throw 'Boundary runner assertion failed.' }; $script:assertions++ }
function Assert-Rejected([scriptblock]$Action) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    Assert $rejected
}
$arguments = @{ TargetDir = (Join-Path $env:TEMP 'plan-target'); Output = (Join-Path $env:TEMP 'plan-output'); WhatIf = $true }
$plan = & $runner @arguments -BuildOnly | ConvertFrom-Json
Assert $plan.build_only
Assert ($plan.adb_port -eq 5037 -and $plan.owner -eq 'g14-sub2')
Assert ($plan.cargo_arguments -contains 'gir_boundary_bench')
Assert ($plan.cargo_arguments -contains '--locked')
Assert ($plan.cooldown_seconds -ge 60 -and $plan.cleanup)
Assert ($plan.lock_retry_seconds -eq 30 -and $plan.session_budget_seconds -lt 1200)
$plan = & $runner @arguments -SkipBuild -CpuMask 40 -Samples 7 -Loops 12345 | ConvertFrom-Json
Assert $plan.skip_build
Assert ($plan.shell.Contains('ADV_BOUNDARY_SAMPLES=7 ADV_BOUNDARY_LOOPS=12345 taskset 40 timeout 120'))
Assert ($plan.transport_deadline_seconds -eq 45)
Assert-Rejected { & $runner @arguments -BuildOnly -SkipBuild }
Assert-Rejected { & $runner @arguments -CpuMask '80;exit' }
Assert-Rejected { & $runner @arguments -Samples 0 }
Assert-Rejected { & $runner @arguments -TimeoutSeconds 1200 }
. $runner @arguments | Out-Null
$choices = @(Get-CpuChoices @('Cpus_allowed_list: 0-2,4', '0 800', '1 900', '2 900', '4 1800', '7 2400'))
Assert (($choices.core -join ',') -eq '4,2,1,0')
Assert-Rejected { Get-CpuChoices @('0 800') }
Assert-Rejected { Get-CpuChoices @('Cpus_allowed_list: 63', '63 900') }
Assert-Rejected { Get-Command '/data/local/tmp/unsafe;exit' }
$text = "case,sample,ns_per_boundary,code_bytes,body_bytes,bridge_bytes,bodies,move_like_est,stack_memory_est,frame_like_est,verified_boundaries,full_state_assertions,checks,frame_pointers`n"
foreach ($case in 0..14) { $text += "case_$case,0,1.000000,32,16,16,1,0,0,0,100,19,0,true`n" }
$text += '# completed samples=15 cases=15 verified_boundaries=1500 full_state_assertions=285'
Assert (@(Test-BenchmarkOutput $text 1 100).Count -eq 17)
Assert-Rejected { Test-BenchmarkOutput $text 7 100 }
Assert-Rejected { Test-BenchmarkOutput $text 1 99 }
Assert-Rejected { Test-BenchmarkOutput ($text + "`nprivate unexpected output") 1 100 }
$testLock = Join-Path $env:TEMP ('boundary-lock-test-' + [Guid]::NewGuid().ToString('N'))
$owned = $false
try {
    $owned = Try-DeviceLock $testLock
    Assert $owned
    Assert ((Get-Content -LiteralPath (Join-Path $testLock 'owner.txt') -Raw).Trim() -eq 'g14-sub2')
    Assert (-not (Try-DeviceLock $testLock))
    Assert ((Get-Content -LiteralPath (Join-Path $testLock 'owner.txt') -Raw).Trim() -eq 'g14-sub2')
    Set-Content -LiteralPath (Join-Path $testLock 'owner.txt') -Value 'another-task'
    Assert-Rejected { Remove-OwnedLock $testLock }
    Assert (Test-Path -LiteralPath (Join-Path $testLock 'owner.txt'))
    Set-Content -LiteralPath (Join-Path $testLock 'owner.txt') -Value 'g14-sub2'
    Remove-OwnedLock $testLock
    $owned = $false
    Assert (-not (Test-Path -LiteralPath $testLock))
} finally {
    if ($owned) { Remove-OwnedLock $testLock }
}
Write-Output "Boundary runner checks: $assertions passed."
