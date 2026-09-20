# Changelog
Todas as mudanças notáveis neste projeto serão documentadas aqui.

O formato é baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.0.0/),
e este projeto adere ao [Semantic Versioning](https://semver.org/lang/pt-BR/).

Podem existir falhas ou itens não declarados, causados por falha humana ou por IA, caso encontre por favor relate para ajudar a manter a consistência dos dados.

As cinco crates — núcleo, engine, adaptador, protocolo e plugin — compartilham a mesma versão e saem no mesmo release.

---

## [0.1.0] - 21/09/2026

Primeira versão. O núcleo reúne o que antes eram dois projetos separados — o
motor de análise (**PawnPro-Engine 1.4.0**) e o depurador
(**PawnPro-Debugger 0.2.1**) — num binário só, e absorve da extensão tudo o que
depende do sistema operacional. Sai junto da **PawnPro 4.0.0**, que é quem o
empacota e o consome.

### Adicionado

#### O núcleo

- **JSON-RPC 2.0 sobre stdio** com a extensão, uma mensagem por linha.
  `core.version` lista os métodos que a versão em execução atende, e um teste
  garante que todo nome listado é despachável.
- **Supervisor dos subsistemas**: cada um roda numa thread com `catch_unwind` na
  borda, volta sozinho quando cai e desiste depois de cinco reinícios que não se
  sustentam; 30 s de pé zeram o contador. A extensão acompanha por
  `core.subsystemStatus`.
- **Soquete local único** (Unix; named pipe no Windows), dentro de um diretório
  `0700`: LSP, DAP e o plugin do servidor chegam pelo mesmo endereço e se
  apresentam na primeira linha (`PAWNPRO/1 lsp|dap|plugin`).
- **Processos, portas e RCON**: uma implementação só no lugar dos caminhos por
  sistema que a extensão mantinha à mão. Toda operação destrutiva passa pelo
  filtro de dono — mesmo executável, mesmo usuário —, inclusive quando o pedido
  vem por RPC.
- **Configuração com um dono**: lê o `config.json` global e o do projeto,
  mescla no JSON bruto, resolve `${workspaceFolder}`, includes, SDK, formatação
  e as listas `.ban`/`.allow`, e entrega pronto — à extensão por RPC, à engine
  por um canal tipado. Um observador confere os arquivos a cada dois segundos;
  um valor de tipo errado é ignorado sem descartar o resto.
- **Compilador**: acha o `pawncc`, pergunta ao binário que flags ele aceita,
  monta a linha de comando e executa em segundo plano, sem prender as outras
  requisições.
- **Estado do projeto** (`.pawnpro/state.json`), gravado de forma atômica e com
  permissão restrita, e coberto por um `.pawnpro/.gitignore` que o núcleo cria.
- **Registro de diagnóstico** em `.pawnpro/logs/`, com níveis
  `off`/`error`/`warn`/`info`, desligado por padrão. Extensão, núcleo e engine
  escrevem no mesmo arquivo, na ordem em que aconteceu.
- **Avisos de terceiros**: o release publica `pawnpro-core-THIRD-PARTY.txt` e
  `pawnpro_debug-THIRD-PARTY.txt`, gerados pelo `cargo-about`. Uma dependência
  com licença fora de `about.toml` impede o release.

#### A engine, agora como biblioteca

- **Unidade de compilação**: hover, assinatura, autocomplete, referências,
  contador e a verificação de símbolos não usados enxergam o que é compilado
  junto com o arquivo — o `.inc` irmão entra, inclusive fechado, e um programa
  à parte nunca, mesmo com funções de mesmo nome.
- **Ir para definição** (`textDocument/definition`): o arquivo, os includes e o
  resto da unidade, nessa ordem; num `forward`, vai ao corpo.
- **Renomear com escopo**: um local vale até o fim do bloco que o declara, e um
  parâmetro, na própria função. Renomear uma `public` atualiza as strings que
  são exatamente o nome, como em `SetTimer("Nome", …)`; renomear uma global com
  homônimo local é recusado, dizendo onde está o conflito.
- **Análise sobre o texto não salvo**, e diagnósticos publicados com a versão do
  documento: avisos de um texto que já não existe deixam de aparecer.
- **Cache de identificadores por arquivo**: a análise do projeto de teste, com
  84 includes, caiu de 0,95 s para cerca de 0,2 s por edição.
- **Sonda contra um projeto real** (`PAWNPRO_PROBE_PROJECT`): passa cada arquivo
  pela análise, pelos tokens semânticos e pela formatação, e cobra que nada
  entre em pane, que a formatação não altere código nem comentário e que
  formatar de novo não mude nada.

#### O depurador

- **Adaptador DAP** (`crates/debugger/adapter`) como biblioteca do núcleo: cada
  sessão roda numa thread, sobe o servidor do jogo e o derruba ao terminar. O
  servidor é filho da sessão, e no Linux morre junto por `PR_SET_PDEATHSIG`.
- **Plugin do servidor** (`pawnpro_debug.so` / `.dll`) e o protocolo entre os
  dois passam a viver aqui e saem no mesmo release. **A depuração é instável.**
- **Só o programa depurado para**: o plugin reconhece a VM pelo conteúdo do
  `.amx`; um filterscript deixa de disparar os breakpoints do gamemode.
- **Breakpoints com o fonte editado**: o texto de cada arquivo é guardado na
  compilação, e a linha do editor é levada à do binário em execução por diff.
  Um `.amx` sem bloco de debug é recompilado no início e no reinício.
- **Arrays** de várias dimensões e recebidos por referência na inspeção, no data
  breakpoint e na edição, que só é dada como feita quando o plugin confirma.
- **Erros de runtime** pausam com a mensagem no idioma configurado.

### Alterado, em relação aos projetos separados

- **A engine deixa de ser um binário próprio** e vira biblioteca: `serve` recebe
  o par de fluxos e o canal de configuração de quem chama.
- **A engine deixa de ler configuração do disco.** `initializationOptions` e
  `workspace/didChangeConfiguration` não são mais fontes: dois leitores do mesmo
  arquivo eram dois donos livres para discordar.
- **O adaptador deixa de ser um binário próprio**, e o plugin passa a ser
  cliente do núcleo em vez de servidor de um canal próprio.
- **Licença própria**: PawnPro-Core License v1.0, valendo para as cinco crates.

### Corrigido, em relação aos projetos separados

- **A formatação danificava o código** em comentários no fim da linha e de
  bloco, em strings e caracteres com espaço, e em diretivas e macros com
  continuação `\`; e não era estável. Nos 270 arquivos do projeto de teste:
  nenhum dano, nenhuma instabilidade.
- **Diretiva recuada**: o léxico só reconhecia `#` na coluna 0, e
  `    #include` virava pontuação solta.
- **`native`/`forward` com corpo na linha seguinte** não eram acusados.
- **Pane na análise e na assinatura** com letras acentuadas: um nome cortado no
  limite de 31 bytes, e a linha cortada por byte antes do cursor.
- **Referências e contador**: um nome dentro de uma string contava como
  referência; uma chamada em `new x = Nome();` era ignorada; a coluna ficava
  errada com acento antes do nome na linha.
- **Renomear um parâmetro** alterava todo nome igual da unidade inteira.
- **`PP0012`** não disparava, e a verificação de não usados não via uma edição
  feita em outro arquivo.
- **Títulos das correções rápidas** eram texto fixo em português; agora seguem o
  idioma configurado, como os diagnósticos.
- **Breakpoint parando na linha errada** com o arquivo editado depois de
  compilar, e **breakpoints do gamemode disparando em filterscripts**.
- **Elementos de array** de várias dimensões ou por referência mostravam o valor
  de outra posição da memória.
