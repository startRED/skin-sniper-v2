# Compila o painel e o programa e instala na pasta do programa, religando-o sem janela.
# Uso: powershell -File scripts/instalar.ps1 [-Destino <pasta>]
param([string]$Destino = (Join-Path (Split-Path $PSScriptRoot -Parent | Split-Path -Parent) 'v2-app'))
# Sem 'Stop' global: no PowerShell 5 o stderr do cargo/npm viraria erro; o código de saída decide.
$raiz = Split-Path $PSScriptRoot -Parent

Push-Location (Join-Path $raiz 'web')
npm ci --silent; if ($LASTEXITCODE) { throw 'npm ci falhou' }
npm run build;   if ($LASTEXITCODE) { throw 'build do painel falhou' }
Pop-Location
Push-Location $raiz
cargo build --release; if ($LASTEXITCODE) { throw 'cargo build falhou' }
Pop-Location

New-Item -ItemType Directory -Force $Destino | Out-Null
Get-Process skin-sniper -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq (Join-Path $Destino 'skin-sniper.exe') } | Stop-Process
Start-Sleep 1
Copy-Item (Join-Path $raiz 'target\release\skin-sniper.exe') $Destino -Force -ErrorAction Stop
Copy-Item (Join-Path $PSScriptRoot 'Iniciar Skin Sniper.vbs') $Destino -Force -ErrorAction Stop
Start-Process (Join-Path $Destino 'skin-sniper.exe') -WorkingDirectory $Destino -WindowStyle Hidden
Write-Host "Instalado em $Destino e ligado: http://localhost:8788"
