$ErrorActionPreference = 'Stop'
$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Venv = Join-Path $Here '.venv'
$Python = $null

if (Get-Command py -ErrorAction SilentlyContinue) {
  $Python = 'py -3'
} elseif (Get-Command python -ErrorAction SilentlyContinue) {
  $Python = 'python'
} else {
  throw 'Python 3.10+ is required for the local OCR/NER worker. Install Python, then run this script again.'
}

if (-not (Test-Path $Venv)) {
  Write-Host 'Creating local ML virtual environment...'
  Invoke-Expression "$Python -m venv `"$Venv`""
}

$VenvPython = Join-Path $Venv 'Scripts\python.exe'
$Stamp = Join-Path $Venv '.email-triage-ml-ready-v0122'
if (-not (Test-Path $Stamp)) {
  Write-Host 'Installing/updating PaddleOCR + GLiNER CPU dependencies for 0.1.22.'
  & $VenvPython -m pip install --upgrade pip
  & $VenvPython -m pip install -r (Join-Path $Here 'requirements.txt')
  New-Item -ItemType File -Path $Stamp -Force | Out-Null
}

# Work around the Paddle 3.x Windows oneDNN/PIR executor failure observed in 0.1.21.
$env:FLAGS_use_mkldnn = '0'
$env:FLAGS_use_onednn = '0'
$env:FLAGS_enable_pir_api = '0'
$env:OMP_NUM_THREADS = '4'

Write-Host 'Starting local OCR/NER worker 0.1.22 on 127.0.0.1:8765.'
Write-Host 'Paddle oneDNN/PIR execution is disabled for Windows compatibility.'
& $VenvPython (Join-Path $Here 'worker.py') --preload
