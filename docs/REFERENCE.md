# Referencia local: tres adaptacoes CGBJ

Checkpoint sucessor de ede4cbb, escolhido pelo usuario em26/11/2026.
Inclui cacheRTV/DSV,recuperacao de orcamento indireto econtrole deworkers.
FonteGPL3 fork-cgbj f4b48ad7;creditos emCREDITS.md. IA:OpenAI Codex,GPT-6.

Abrir Iniciar-Referencia.cmd. PerfilSeriesS,janela1280x720,bindcache1,
indirectrecover1,pipelineworkers4,pipelinelowest1,comperfilbufferpass docheckpoint.
O iniciador usa target/release/victory-reference.exe,ou victory.exe aposbuild.
Build: cargo build --release --locked -p victory.

O teste conjunto anterior mediu5,2FPS/193ms em60s,contra4,65FPS/215,35ms
comsomente cache. Comparacao observacional;naoha ganhocausalcomprovado.
Teste de imagem e estabilidade do novo checkpoint continua necessario.
Shaderguards,invalidbatch,entrydedup eprofiling anteriores nao incluidos.
Nenhumdos3recursos seguintes escolhidosparaestudo foiportado.
Referenciavictory-buffer-pass.exe/ede4cbb preservada pararrollback.
