$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$log = Join-Path $env:EVIDENCE 'warp-probe.log'
$build = (New-Item -Force -ItemType Directory -Path "$env:RUNNER_TEMP/xcl-warp-probe").FullName
$locator = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vs = & $locator -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'Visual Studio x64 build tools are required.' }
Import-Module (Join-Path $vs 'Common7/Tools/Microsoft.VisualStudio.DevShell.dll')
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'
Push-Location -LiteralPath $build
try {
    cl.exe /nologo /EHsc /std:c++17 (Join-Path $PSScriptRoot 'warp-probe.cpp') /Fewarp-probe.exe /link d3d12.lib dxgi.lib 2>&1 |
        Tee-Object -FilePath $log
    ./warp-probe.exe | Tee-Object -FilePath $log -Append
} finally {
    Pop-Location
}
