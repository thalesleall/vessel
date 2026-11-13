# Vessel test lab helper (Windows PowerShell 5.1+).
#
#   powershell -File deploy\dev\lab.ps1 up         build and start agent + SSH servers
#   powershell -File deploy\dev\lab.ps1 status     containers and health
#   powershell -File deploy\dev\lab.ps1 creds      show the test credentials
#   powershell -File deploy\dev\lab.ps1 rekey      change the bastion's host key (tests the "key changed" alert)
#   powershell -File deploy\dev\lab.ps1 restart    restart the agent (data must survive)
#   powershell -File deploy\dev\lab.ps1 bot <cmd>  run vessel-dev as the test account (bot@vessel.test)
#   powershell -File deploy\dev\lab.ps1 logs       follow the agent's logs
#   powershell -File deploy\dev\lab.ps1 down       stop everything (keeps data)
#   powershell -File deploy\dev\lab.ps1 reset      stop and DELETE all lab data and credentials

param(
    [Parameter(Position = 0)] [string] $Command = "status",
    [Parameter(Position = 1, ValueFromRemainingArguments = $true)] [string[]] $Rest
)
$ErrorActionPreference = "Stop"
$Lab = $PSScriptRoot
$Root = Resolve-Path (Join-Path $Lab "..\..")
$Compose = @("compose", "-f", (Join-Path $Lab "docker-compose.yml"))
$Server = "http://localhost:8080"
$BotEmail = "bot@vessel.test"

function Dc { & docker @Compose @args; if ($LASTEXITCODE -ne 0) { throw "docker compose $args failed" } }

function Bot {
    $pw = Get-Content (Join-Path $Lab ".lab\bot-password") -Raw
    $env:VESSEL_DEV_PASSWORD = $pw.Trim()
    Push-Location $Root
    try {
        cargo run -q -p vessel-dev -- --db deploy/dev/.lab/bot.db @args
    } finally { Pop-Location; Remove-Item Env:VESSEL_DEV_PASSWORD }
}

switch ($Command) {
    "up" {
        Dc up -d --build
        Write-Host "Aguardando o agente..."
        for ($i = 0; $i -lt 60; $i++) {
            try { if ((Invoke-WebRequest "$Server/health" -UseBasicParsing -TimeoutSec 2).StatusCode -eq 200) { break } } catch {}
            Start-Sleep -Seconds 1
        }
        Write-Host ""
        Write-Host "Agente ......... $Server   (use esta URL na tela Conectar do app)"
        Write-Host "SSH bastion .... 127.0.0.1 porta 2222, usuario dev"
        Write-Host "SSH alvo ....... endereco 'target' porta 22, so via bastion (jump host)"
        Write-Host "Credenciais .... $Lab\.lab\CREDENCIAIS.txt"
    }
    "status" { Dc ps; try { Write-Host ("health: " + (Invoke-WebRequest "$Server/health" -UseBasicParsing -TimeoutSec 2).Content) } catch { Write-Host "health: agente fora do ar" } }
    "creds" { Get-Content (Join-Path $Lab ".lab\CREDENCIAIS.txt") }
    "rekey" {
        Dc exec -u root bastion sh -c "rm -f /etc/ssh/ssh_host_* && ssh-keygen -A"
        Dc restart bastion
        Write-Host "Chave de host do bastion trocada. A proxima conexao deve mostrar o alerta de chave alterada."
    }
    "restart" { Dc restart agent }
    "logs" { Dc logs -f agent }
    "bot" {
        if (-not $Rest) { Bot; break }
        if ($Rest[0] -eq "signup") { Bot signup $Server $BotEmail; break }
        Bot @Rest
    }
    "down" { Dc down }
    "reset" {
        Dc down -v
        Remove-Item -Recurse -Force (Join-Path $Lab ".lab") -ErrorAction SilentlyContinue
        Write-Host "Laboratorio apagado (dados do agente, credenciais e conta de teste)."
    }
    default { throw "comando desconhecido: $Command" }
}
