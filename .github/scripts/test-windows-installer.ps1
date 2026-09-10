[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$BundleDirectory,

  [Parameter(Mandatory = $true)]
  [string]$InstallDirectory,

  [switch]$RequireSignature
)

$ErrorActionPreference = "Stop"

$installers = @(Get-ChildItem -Path $BundleDirectory -Filter "*-setup.exe" -File -Recurse)
if ($installers.Count -ne 1) {
  throw "Expected exactly one NSIS installer below '$BundleDirectory'; found $($installers.Count)."
}

$installer = $installers[0]
Write-Host "Installing $($installer.Name) into $InstallDirectory"
$installerProcess = Start-Process -FilePath $installer.FullName `
  -ArgumentList @("/S", "/D=$InstallDirectory") `
  -Wait -PassThru
if ($installerProcess.ExitCode -ne 0) {
  throw "The installer exited with code $($installerProcess.ExitCode)."
}

$installedExecutables = @(Get-ChildItem -Path $InstallDirectory -Filter "*.exe" -File)
$applicationExecutables = @($installedExecutables | Where-Object { $_.Name -ne "uninstall.exe" })
if ($applicationExecutables.Count -ne 1) {
  throw "Expected one installed application executable; found $($applicationExecutables.Count)."
}

$application = $applicationExecutables[0]
if ($RequireSignature) {
  $signature = Get-AuthenticodeSignature -FilePath $application.FullName
  if ($signature.Status -ne "Valid") {
    throw "The installed application does not have a valid Authenticode signature: $($signature.Status)."
  }
}

Write-Host "Launching $($application.FullName)"
$applicationProcess = Start-Process -FilePath $application.FullName -PassThru
Start-Sleep -Seconds 5
if ($applicationProcess.HasExited) {
  throw "The installed application exited during its startup smoke test (exit code $($applicationProcess.ExitCode))."
}

Stop-Process -Id $applicationProcess.Id -Force
