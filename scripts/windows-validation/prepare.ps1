$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$pairPath = Join-Path $PSScriptRoot 'revisions.json'
Copy-Item -LiteralPath $pairPath -Destination "$env:EVIDENCE/revisions.json"
$pair = Get-Content -Raw -LiteralPath $pairPath | ConvertFrom-Json
foreach ($revision in @($pair.baseline, $pair.candidate)) {
    if ($revision -cnotmatch '\A[0-9a-f]{40}\z') { throw "Expected a full commit SHA: $revision" }
    git merge-base --is-ancestor $revision HEAD
}
$parent = git rev-list --parents -n 1 $pair.candidate
if ($parent -cne "$($pair.candidate) $($pair.baseline)") {
    throw 'The candidate must be a single commit directly on the baseline.'
}
$patch = Join-Path $env:EVIDENCE 'candidate.patch'
git -c core.quotePath=true diff --binary --no-color --no-ext-diff --no-textconv --abbrev=7 $pair.baseline $pair.candidate -- > $patch
$actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $patch).Hash.ToLowerInvariant()
"patch_sha256=$actual" | Tee-Object -FilePath "$env:EVIDENCE/source-identity.log"
if ($actual -cne $pair.patch_sha256) { throw "Candidate patch hash mismatch: $actual" }

$os = Get-CimInstance Win32_OperatingSystem
"windows=$($os.Caption) version=$($os.Version) build=$($os.BuildNumber) image=$env:ImageOS image_version=$env:ImageVersion" |
    Tee-Object -FilePath "$env:EVIDENCE/environment.log"
"arch=$([Runtime.InteropServices.RuntimeInformation]::OSArchitecture) powershell=$($PSVersionTable.PSVersion) gaming_services_dll=$(Test-Path "$env:SystemRoot/System32/xgameruntime.dll")" |
    Tee-Object -FilePath "$env:EVIDENCE/environment.log" -Append
Get-CimInstance Win32_Processor | ForEach-Object { "cpu=$($_.Name)" } |
    Tee-Object -FilePath "$env:EVIDENCE/environment.log" -Append
rustup toolchain install stable-x86_64-pc-windows-msvc --profile minimal 2>&1 |
    Tee-Object -FilePath "$env:EVIDENCE/environment.log" -Append
$rust = rustc +stable-x86_64-pc-windows-msvc -Vv
$rust | Tee-Object -FilePath "$env:EVIDENCE/environment.log" -Append
if ($rust -cnotcontains 'host: x86_64-pc-windows-msvc') { throw 'Expected the native MSVC toolchain.' }
cargo +stable-x86_64-pc-windows-msvc -V | Tee-Object -FilePath "$env:EVIDENCE/environment.log" -Append
