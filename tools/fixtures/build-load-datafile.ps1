param([string]$OutputDirectory=(Join-Path $PSScriptRoot '../../target/load-datafile-tests'))
$ErrorActionPreference='Stop'
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$dataSdk='C:/Program Files (x86)/Windows Kits/10/Lib/10.0.26100.0/um/x86'
$dataLlvm='C:/Program Files/LLVM/bin'
function Invoke-DataChecked([string]$program,[string[]]$arguments) {
    & $program @arguments
    if($LASTEXITCODE -ne 0){throw "$program failed ($LASTEXITCODE)"}
}
$dataCompile=@('/nologo','/c','/O2','/GS-','/Gs9999999','/Zl','/clang:-fno-builtin','/clang:--target=i686-pc-windows-msvc',"/I$PSScriptRoot")
$dataLink=@('/nologo','/machine:x86','/nodefaultlib','/dynamicbase','/safeseh:no','/timestamp:0',"/libpath:$dataSdk","/libpath:$OutputDirectory")
Invoke-DataChecked "$dataLlvm/clang-cl.exe" ($dataCompile+@('/DDEPENDENCY',"$PSScriptRoot/load_datafile_dll.c","/Fo$OutputDirectory/dep.obj"))
Invoke-DataChecked "$dataLlvm/lld-link.exe" ($dataLink+@('/dll','/base:0x12000000','/entry:DllMain@12',"/out:$OutputDirectory/special_dep.dll","/implib:$OutputDirectory/special_dep.lib","$OutputDirectory/dep.obj",'kernel32.lib'))
Invoke-DataChecked "$dataLlvm/llvm-rc.exe" @('/nologo',"/fo$OutputDirectory/data.res","$PSScriptRoot/resource_handles.rc")
Invoke-DataChecked "$dataLlvm/clang-cl.exe" ($dataCompile+@("$PSScriptRoot/load_datafile_dll.c","/Fo$OutputDirectory/dll.obj"))
Invoke-DataChecked "$dataLlvm/lld-link.exe" ($dataLink+@('/dll','/base:0x10000000','/entry:DllMain@12',"/out:$OutputDirectory/datafile.dll","$OutputDirectory/dll.obj","$OutputDirectory/data.res",'special_dep.lib','kernel32.lib'))
# A non-page-aligned file tail exercises the actual file length, not just the
# page-rounded mapping size. The extra byte is authored fixture padding.
$dataPadding=[IO.File]::Open((Join-Path $OutputDirectory 'datafile.dll'),[IO.FileMode]::Append,[IO.FileAccess]::Write)
try{$dataPadding.WriteByte(0)}finally{$dataPadding.Dispose()}
foreach($dataVariant in @('native','unsupported')) {
    $dataFlags=if($dataVariant -eq 'unsupported'){@('/DADV_UNSUPPORTED')}else{@()}
    Invoke-DataChecked "$dataLlvm/clang-cl.exe" ($dataCompile+$dataFlags+@("$PSScriptRoot/load_datafile.c","/Fo$OutputDirectory/$dataVariant.obj"))
    Invoke-DataChecked "$dataLlvm/lld-link.exe" ($dataLink+@('/subsystem:console','/entry:mainCRTStartup',"/out:$OutputDirectory/fixture_datafile_$dataVariant.exe","$OutputDirectory/$dataVariant.obj",'kernel32.lib'))
}
