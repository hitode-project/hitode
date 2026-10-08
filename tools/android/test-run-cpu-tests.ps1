<#
.SYNOPSIS
Check Android CPU runner plans without the SDK, NDK, adb or a device.
.DESCRIPTION
Run ./tools/android/test-run-cpu-tests.ps1. Inputs are fixed synthetic target
and filter selections. Output is an assertion count; failure throws. No build,
device action or output directory is allowed during these dry-run checks.
#>
$ErrorActionPreference = 'Stop'
$runner = Join-Path $PSScriptRoot 'run-cpu-tests.ps1'
$assertions = 0
function Assert($condition, $message) {
    if (-not $condition) { throw $message }
    $script:assertions++
}
function Reject($action, $message) {
    $rejected = $false
    try { & $action | Out-Null } catch { $rejected = $true }
    Assert $rejected $message
}
$output = Join-Path $PSScriptRoot ('../../target/scratch/android-ready/dry-' + [Guid]::NewGuid().ToString('N'))
$environment = @{}
foreach ($name in @('ANDROID_NDK_HOME', 'ANDROID_NDK_ROOT', 'ANDROID_HOME', 'ANDROID_SDK_ROOT', 'CMAKE_TOOLCHAIN_FILE', 'CARGO_BUILD_JOBS', 'PATH')) {
    $environment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$defaults = & $runner -WhatIf -Sdk '' -Ndk '' -Output $output | ConvertFrom-Json
Assert (($defaults.targets -join ',') -eq 'execution_contract,host_interrupts,loop_versioning,address_space') 'Default integration targets changed.'
Assert ($defaults.runs.Count -eq 4) 'Default run count changed.'
Assert ($defaults.cargo_arguments -contains '--locked') 'Build must use the lockfile.'
$oldUnit = & $runner -WhatIf -TestName @() -UnitFilter virtual_memory::tests -Output $output | ConvertFrom-Json
Assert (($oldUnit.targets -join ',') -eq 'adv_cpu') 'Unit-only command must build one ELF.'
Assert ($oldUnit.runs[0].id -eq 'adv_cpu') 'Single-filter log name must stay compatible.'
$all = & $runner -ExecutionExperiments -WhatIf -Serial 'test-device' -AdbPort 15037 -TimeoutSeconds 600 -JitVerify strict -Output $output | ConvertFrom-Json
Assert (($all.targets -join ',') -eq 'tier1,adv_cpu') 'Experiments must build Tier 1 and the unit harness.'
Assert ($all.runs.Count -eq 5) 'A/B/C/D and code publication must all run.'
Assert (($all.runs.filter -join ',') -eq ',virtual_memory,fault_recovery,tier1::native::publication_tests,compile::x87::tests') 'Experiment filters are incomplete.'
Assert (@($all.runs.id | Select-Object -Unique).Count -eq 5) 'Unit filters must have separate evidence logs.'
Assert ($all.serial -eq 'test-device' -and $all.adb_port -eq 15037) 'Device selection was not retained.'
foreach ($run in $all.runs) {
    Assert ($run.shell.Contains('ADV_JIT_VERIFY=strict timeout 600')) 'Strict mode or timeout did not reach the device command.'
    Assert ($run.shell.Contains('--test-threads=1 --nocapture')) 'Harness options changed.'
    Assert ($run.shell.Contains('unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER CARGO_TARGET_AARCH64_LINUX_ANDROID_RUNNER ADV_CPU_DIRECT_PROBE ADV_CONTROLLED_FAULT_CASE ADV_JIT_VERIFY')) 'Parent fixture environment is not isolated.'
    Assert (-not $run.shell.Contains('--ignored')) 'Expected-signal fixture must only run under its parent.'
}
$build = & $runner -ExecutionExperiments -BuildOnly -WhatIf -Output $output | ConvertFrom-Json
Assert $build.build_only 'Build-only selection was lost.'
$multiple = & $runner -TestName tier1,tier1 -UnitFilter virtual_memory,fault_recovery,virtual_memory -WhatIf -Output $output | ConvertFrom-Json
Assert ($multiple.targets.Count -eq 2 -and $multiple.runs.Count -eq 3) 'Duplicate selectors should run only once.'
$full = & $runner -AllTests -DirectMemory -Tier1 bridge -WhatIf -Output $output | ConvertFrom-Json
$expected = @(Get-ChildItem (Join-Path $PSScriptRoot '../../crates/adv-cpu/tests') -File -Filter '*.rs')
Assert ($full.targets.Count -eq $expected.Count + 1) 'Full suite must include every integration target and the library.'
Assert ($full.runs[-1].target -eq 'adv_cpu' -and $full.runs[-1].filter -eq '') 'Full library suite must have no filter.'
Assert ($full.runs[0].shell.Contains('ADV_TIER1=bridge ADV_DIRECT_MEMORY=1 timeout')) 'Execution settings must reach the device.'
Reject { & $runner -AllTests -ExecutionExperiments -WhatIf } 'Conflicting presets were accepted.'
Reject { & $runner -AllTests -UnitFilter virtual_memory -WhatIf } 'Full suite accepted a unit filter.'
Reject { & $runner -WhatIf -TestName @() } 'Empty selection was accepted.'
Reject { & $runner -WhatIf -TestName missing_target } 'Unknown integration target was accepted.'
Reject { & $runner -WhatIf -UnitFilter 'fault;exit' } 'Shell metacharacters were accepted.'
Reject { & $runner -WhatIf -ExecutionExperiments -TestName tier1 } 'Conflicting selectors were accepted.'
Assert (-not (Test-Path -LiteralPath $output)) 'Dry-run created output files.'
foreach ($name in $environment.Keys) {
    Assert ([Environment]::GetEnvironmentVariable($name, 'Process') -eq $environment[$name]) "Dry-run modified $name."
}
Write-Output "Android CPU runner dry-run checks: $assertions passed."
