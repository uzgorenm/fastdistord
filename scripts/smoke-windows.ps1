$ErrorActionPreference = 'Stop'
$installer = (Resolve-Path 'dist/Fastdistord-0.02-windows-x64-setup.exe').Path
$stage = Join-Path $env:RUNNER_TEMP 'fastdistord-install-smoke'
$p = Start-Process -FilePath $installer -ArgumentList @('/S', "/D=$stage") -Wait -PassThru
if ($p.ExitCode -ne 0) { throw "Installer exit $($p.ExitCode)" }
$installed = Join-Path $stage 'fastdistord.exe'
if ((Get-FileHash $installed).Hash -ne (Get-FileHash 'target/release/fastdistord.exe').Hash) { throw 'Installed payload hash mismatch' }
$output = Join-Path $env:RUNNER_TEMP 'fastdistord-version.txt'
$p = Start-Process -FilePath $installed -ArgumentList '--version' -RedirectStandardOutput $output -Wait -PassThru
if ($p.ExitCode -ne 0 -or (Get-Content $output -Raw).Trim() -ne 'fastdistord 0.02') { throw 'Installed binary version smoke failed' }
$uninstaller = Join-Path $stage 'Uninstall.exe'
$p = Start-Process -FilePath $uninstaller -ArgumentList @('/S', "_?=$stage") -Wait -PassThru
if ($p.ExitCode -ne 0 -or (Test-Path $installed)) { throw 'Uninstall smoke failed' }
Get-FileHash $installer -Algorithm SHA256 | Format-List
