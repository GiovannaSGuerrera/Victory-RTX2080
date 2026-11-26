# Estado de desenvolvimento — 25/11/2026

## Resultado confirmado localmente

O runtime compilou em Release, abriu sua janela, resolveu os imports da
sessão validada e apresentou conteúdo 3D real. Foram observados personagens,
cabelo, roupas, vegetação, construções e iluminação. O usuário confirmou
conseguir jogar. Não foi validada a conclusão de uma missão.

Ambiente observado: Windows 11, NVIDIA RTX 2080 8 GB, aproximadamente 32 GB
de RAM, modo Series S, janela com área cliente 1280×720. Os resultados são
de uma máquina, com cache local já produzido.

## Referência recuperável

- Commit do perfil jogável: `f7f765c`.
- Tag: `checkpoint-jogavel-2026-11-25`.
- Binário local: SHA256
  `4103a7165270694acd4b934ae5656812a9625f3a5efc6192842de4886a0a4470`.
- Launcher e configuração de referência em `scripts/`.
- Instruções e manifesto em `docs/local-playable-checkpoint.md` e
  `docs/local-playable-checkpoint.json`.

O código do renderer não foi alterado para produzir esse checkpoint local.
O trabalho registrado compreende compilação, configuração, diagnóstico,
verificação visual e preservação do estado recuperável.

## Experimentos e achados

1. A primeira tentativa apresentou tela preta e cerca de 1200 pipelines
   pendentes, além de draws descartados.
2. Foi habilitado `VICTORY_PIPELINE_SMALL_FIRST=1`, sem trocar o binário ou
   apagar o cache. A sessão seguinte confirmou o escalonador ativo e depois
   apresentou cenário real. Como houve reaproveitamento de cache, não foi
   isolada a causalidade dessa opção.
3. Em sessão posterior, a fila chegou a zero e a lentidão continuou.
   Uma amostra registrou 1945 ms/frame, 1570 ms em preparação de texturas e
   44 ms na GPU. São medições do runtime; não um benchmark controlado.
4. O ajuste `VICTORY_BAKE_TEXTURES_MS=0` foi preparado para investigar esse
   custo, mas não validado na sessão confirmada como jogável. Continua
   disponível somente com `-ExperimentalBakeBudget`.

## Problemas restantes

- FPS baixo e pausas prolongadas na preparação de recursos.
- Pipelines recusados, draws descartados e imagem incompleta.
- Exposição excessiva, superfícies pretas e vegetação pontilhada.
- Pressão de memória observada; não foi provada como causa única de falhas.
- Crash anterior com exceção `0xC0000005`, escrita em endereço nulo, ainda
  sem causa efetiva corrigida.
- Estabilidade prolongada, missão completa e fidelidade visual não validadas.

## Verificações realizadas

Build Release com `--locked --offline`: exit 0. Leitura da configuração pelo
CLI: exit 0. Scripts PowerShell passaram no parser. Política de arquivos e
checagem de whitespace passaram para os commits locais. A suíte completa
de testes e o clippy não foram declarados aprovados nesta etapa.

Logs, saves, cache, binários e backups continuam fora deste repositório.
O Git preserva código e configuração; não salva o progresso da missão.

## Próximo passo

Reproduzir a mesma cena, medir custos de texturas e sincronização e testar
uma mudança por vez, preservando o checkpoint. Melhorias de desempenho não
devem ser confundidas com correção dos defeitos visuais.
