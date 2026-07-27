$ErrorActionPreference = "Stop"
$signingDir = Join-Path $env:RUNNER_TEMP "photoproof-windows-signing"
$certificatePath = Join-Path $signingDir "certificate.pfx"
New-Item -ItemType Directory -Force -Path $signingDir | Out-Null
[IO.File]::WriteAllBytes(
  $certificatePath,
  [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE)
)
$password = ConvertTo-SecureString `
  -String $env:WINDOWS_CERTIFICATE_PASSWORD `
  -AsPlainText `
  -Force
$certificate = Import-PfxCertificate `
  -FilePath $certificatePath `
  -CertStoreLocation "Cert:\CurrentUser\My" `
  -Password $password
if (-not $certificate.Thumbprint) {
  throw "Windows signing certificate import produced no thumbprint"
}
"WINDOWS_CERTIFICATE_THUMBPRINT=$($certificate.Thumbprint)" |
  Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
