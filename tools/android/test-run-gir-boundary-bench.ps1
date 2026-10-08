<#
.SYNOPSIS
Test boundary benchmark plans, parsers and scheduled release without a device.
.DESCRIPTION
Usage: ./tools/android/test-run-gir-boundary-bench.ps1 after setting task TEMP/TMP.
Inputs: runner and task-local TEMP. Output: assertion count. Scheduler calls are
mocked in memory. No build, device, APK, filesystem writes or shared-lock access.
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
Assert ($plan.adb_port -eq 5037 -and $plan.owner -eq 'g14')
Assert ($plan.cargo_arguments -contains 'gir_boundary_bench')
Assert ($plan.cargo_arguments -contains '--locked')
Assert (-not $plan.keep_device_queue -and $plan.cleanup)
Assert ($plan.session_budget_seconds -lt 1200)
$plan = & $runner @arguments -SkipBuild -CpuMask 40 -Samples 7 -Loops 12345 -Task g14-sub2 -DeviceHelper scheduler.ps1 -KeepDeviceQueue | ConvertFrom-Json
Assert $plan.skip_build
Assert ($plan.keep_device_queue -and $plan.owner -eq 'g14-sub2' -and $plan.device_helper -eq 'scheduler.ps1')
Assert ($plan.shell.Contains('ADV_BOUNDARY_SAMPLES=7 ADV_BOUNDARY_LOOPS=12345 taskset 40 timeout 120'))
Assert ($plan.transport_deadline_seconds -eq 45)
Assert-Rejected { & $runner @arguments -BuildOnly -SkipBuild }
Assert-Rejected { & $runner @arguments -CpuMask '80;exit' }
Assert-Rejected { & $runner @arguments -Samples 0 }
Assert-Rejected { & $runner @arguments -TimeoutSeconds 1200 }
Assert-Rejected { & $runner @arguments -Task '../another-task' }
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
function Release-Device([string]$Task, [switch]$Done) {
    $script:releasedTask = $Task
    $script:releasedDone = [bool]$Done
}
Exit-DeviceSession 'g14-sub2' $false
Assert ($releasedTask -eq 'g14-sub2' -and $releasedDone)
Exit-DeviceSession 'g14' $true
Assert ($releasedTask -eq 'g14' -and -not $releasedDone)
Write-Output "Boundary runner checks: $assertions passed."
