param(
    [string]$RuntimePath,
    [string]$OutputDirectory,
    [string]$SourceCommit = 'HEAD'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (!$RuntimePath) { $RuntimePath = Join-Path $repo 'target\release\victory.exe' }
if (!$OutputDirectory) { $OutputDirectory = Join-Path $repo ('target\packages\Victory-RTX2080-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
$RuntimePath = (Resolve-Path -LiteralPath $RuntimePath).Path
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Escolha uma pasta de saida nova; nenhum pacote existente sera substituido.' }
$compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
if (!(Test-Path -LiteralPath $compiler)) { throw 'Compilador .NET Framework 4.x nao encontrado.' }
$package = Join-Path $OutputDirectory 'Victory-RTX2080'
New-Item -ItemType Directory -Path (Join-Path $package 'runtime'),(Join-Path $package 'source') | Out-Null
$launcher = Join-Path $package 'Iniciar-Victory.exe'
& $compiler /nologo /target:winexe /platform:x64 "/out:$launcher" /reference:System.Windows.Forms.dll /reference:System.Xml.Linq.dll /reference:System.Core.dll (Join-Path $PSScriptRoot 'launcher\Launcher.cs')
if ($LASTEXITCODE -ne 0) { throw 'Falha ao compilar iniciador.' }
Copy-Item -LiteralPath $RuntimePath -Destination (Join-Path $package 'runtime\victory.exe')
Copy-Item -LiteralPath (Join-Path $repo 'LICENSE'),(Join-Path $repo 'CREDITS.md') -Destination $package
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'launcher\Launcher.cs') -Destination (Join-Path $package 'source')
& git -C $repo archive --format=zip "--output=$(Join-Path $package 'source\runtime-source.zip')" $SourceCommit
if ($LASTEXITCODE -ne 0) { throw 'Falha ao incluir fontes do runtime.' }
@'
VICTORY RTX2080 - WINDOWS X64

Extraia TODO o pacote e abra Iniciar-Victory.exe com dois cliques.
Na primeira abertura, selecione o executavel do seu pacote autorizado do jogo.
Nas proximas aberturas, a escolha e reutilizada automaticamente.
Para selecionar outro pacote: Iniciar-Victory.exe --configure

O pacote contem somente o runtime e o iniciador, com seus fontes e licenca.
Nao inclui jogo, DLLs Xbox, saves, caches ou arquivos de shaders do jogo.
Precisa de Windows x64, DirectX12, driver compativel e validador DXIL disponivel
(Windows SDK x64, ou caminho proprio em VICTORY_DXIL). Dependencias do pacote
do jogo continuam necessarias. Nao altera verificacoes de integridade.

Perfil: Series S, janela720p, MAPPING_RANGES1, HEAP_PASS_NO_SELF_MARK1.
Profiling desativado. Nao existe promessa de FPS ou compatibilidade universal.
Configuracao, saves, cache e logs: %LOCALAPPDATA%\VictoryRTX2080
Se as pastas locais victory-8gb-cache e victory-data existirem ao lado do
pacote selecionado, elas sao reutilizadas, preservando saves e cache existentes.

Executaveis nao assinados podem receber avisos do Windows; o pacote nao muda
configuracoes de seguranca. Leia LICENSE, CREDITS.md e source antes de usar.
'@ | Set-Content -LiteralPath (Join-Path $package 'LEIA-ME.txt') -Encoding UTF8
$hashes = Get-FileHash -LiteralPath $launcher,(Join-Path $package 'runtime\victory.exe') -Algorithm SHA256
@{ source_commit = $SourceCommit; runtime_sha256 = $hashes[1].Hash; launcher_sha256 = $hashes[0].Hash; profile = 'series-s-720p-buffer-pass'; game_included = $false } |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $package 'package.json') -Encoding UTF8
$zip = Join-Path $OutputDirectory 'Victory-RTX2080-Windows-x64.zip'
Compress-Archive -LiteralPath $package -DestinationPath $zip
Write-Output "Pacote: $zip"
Write-Output "Iniciador: $launcher"
