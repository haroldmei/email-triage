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
$Stamp = Join-Path $Venv '.email-triage-ml-ready'
if (-not (Test-Path $Stamp)) {
  Write-Host 'Installing PaddleOCR + GLiNER CPU dependencies. The first setup is large because model runtimes are downloaded locally.'
  & $VenvPython -m pip install --upgrade pip
  & $VenvPython -m pip install -r (Join-Path $Here 'requirements.txt')
  New-Item -ItemType File -Path $Stamp -Force | Out-Null
}

Write-Host 'Starting local OCR/NER worker on 127.0.0.1:8765.'
Write-Host 'The first model load downloads the open model weights and may take several minutes.'
& $VenvPython (Join-Path $Here 'worker.py') --preload
