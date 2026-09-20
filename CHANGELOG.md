# Changelog
Todas as mudanças notáveis neste projeto serão documentadas aqui.

O formato é baseado em [Keep a Changelog](https://keepachangelog.com/pt-BR/1.0.0/),
e este projeto adere ao [Semantic Versioning](https://semver.org/lang/pt-BR/).

Podem existir falhas ou itens não declarados, causados por falha humana ou por IA, caso encontre por favor relate para ajudar a manter a consistência dos dados.

As três crates — core, engine e depurador — compartilham a mesma versão e são publicadas juntas, num binário só.

---

## [Não lançado]

### Adicionado

- **Núcleo**: JSON-RPC sobre stdio para a extensão, supervisor dos subsistemas, e tudo o que depende do sistema operacional — processos, portas, RCON, compilador e configuração.
- **Hospedagem da engine**: ela sobe numa thread supervisionada e atende LSP num soquete Unix dentro de um diretório `0700` — named pipe no Windows —, com o endereço reservado uma vez e mantido entre reinícios. `engine.start` devolve o endereço.
- **O núcleo é a única fonte da configuração da engine**: lê o `config.json` global e o do projeto, resolve includes, SDK, formatação e as listas `.ban`/`.allow`, e entrega tudo pronto por um canal interno tipado. Um observador confere os arquivos a cada dois segundos e reentrega quando mudam; a engine republica os diagnósticos sem o editor pedir. Trocar de projeto move a observação junto.
- **Registro de diagnóstico** em `.pawnpro/logs/`, com níveis `off`/`error`/`warn`/`info`. Cada evento vai para o `pawnpro.log` unificado e para o arquivo do componente que o produziu. Desligado por padrão: nada é escrito e nenhum arquivo é criado. Métodos `log.configure`, `log.write` e `log.clear`, para a extensão alimentar o mesmo arquivo. A engine registra pelo mesmo caminho, por um sink que o núcleo injeta.
- `read_list_file` passa a respeitar um teto de tamanho, que era da engine.
- **Plugin de depuração do servidor** (`pawnpro_debug.so` / `pawnpro_debug.dll`) e o protocolo que ele fala passam a viver aqui, em `crates/debugger/`, e saem no mesmo release do núcleo — **instáveis para uso**.
- **Configuração, estado, compilador e includes pelo núcleo**: métodos `config.*`, `state.*`, `compiler.buildArgs`/`compiler.detect` e `includes.*` (`paths`, `listFiles`, `listNatives`, `resolveSdk`), cada grupo em módulo próprio. A configuração tem um dono só, que avisa a extensão por `config.changed` quando o arquivo muda; a leitura tolera campo a campo, e um valor inválido cai no padrão sem descartar o resto. Um teste de contrato confere o que a engine recebe.
- **Engine — unidade de compilação.** Hover, assinatura, autocomplete, referências, contador e a verificação de símbolos não usados enxergam o que é compilado junto com o arquivo: cada programa que o inclui, com tudo o que esse programa inclui — o `.inc` irmão entra, inclusive fechado; um programa à parte nunca, mesmo com funções de mesmo nome. Programa é o `.pwn` que nenhum arquivo do projeto inclui: um `.pwn` incluído é trecho.
- **Engine — ir para definição** (`textDocument/definition`): o arquivo, os includes e o resto da unidade, nessa ordem; num `forward`, vai ao corpo.
- **Engine — renomear com escopo.** Um local vale do `new` que o declara até o fim do bloco dele — num `for (new i …)`, só no `for` —, e um parâmetro, na própria função. Renomear uma `public` atualiza também as strings que são exatamente o nome, como em `SetTimer("Nome", …)`. Renomear uma variável global cujo nome também é local ou parâmetro na unidade é recusado, com o motivo e o lugar do conflito, nos cinco idiomas.
- **Soquete único**: LSP, DAP e o plugin do servidor chegam pelo mesmo endereço, e cada conexão diz na primeira linha a que canal pertence (`PAWNPRO/1 lsp|dap|plugin`). O plugin passa a ser cliente, com endpoint e sessão recebidos pelo ambiente.
- **Adaptador DAP no núcleo** (`crates/debugger/adapter`), como biblioteca supervisionada: `debug.start` devolve o endereço, e cada sessão sobe e derruba o servidor do jogo. Parar leva milissegundos.
- **Depurador — breakpoints com o fonte editado**: o texto de cada arquivo é guardado na compilação, e as linhas do editor são levadas às do binário em execução por diff. `.amx` sem bloco de debug é recompilado no início e no reinício.
- **Depurador — só o programa depurado**: o plugin reconhece a VM pelo conteúdo do `.amx` e instala o controle só nela; filterscripts deixam de disparar os breakpoints do gamemode.
- **Depurador — arrays**: elementos de arrays multidimensionais e passados por referência, com data breakpoint e edição confirmada pelo plugin.
- Métodos `compiler.run` (em segundo plano, sem prender as outras requisições), `server.resolve`, `server.readLog`, `server.sensitiveCommands`, `project.changelogSection` e `config.inlineNamingLists`, que substituem as cópias em TypeScript da extensão.
- **Avisos de terceiros**: o release publica `pawnpro-core-THIRD-PARTY.txt` e `pawnpro_debug-THIRD-PARTY.txt`, gerados pelo `cargo-about`; uma licença fora de `about.toml` impede o release.
- **Engine — sonda contra um projeto real** (`PAWNPRO_PROBE_PROJECT`): passa cada arquivo pela análise, pelos tokens semânticos e pela formatação, e cobra que nada entre em pane, que a formatação não mude código nem comentário e que formatar de novo não mude nada. Mede onde vai o tempo da análise e confere dois cenários de unidade de compilação.

### Alterado

- **A engine deixa de ser um binário próprio** e passa a ser biblioteca: `serve` recebe o par de fluxos e o canal de configuração de quem chama. Quem publica o executável agora é o núcleo, e a release traz um binário só.
- **A engine deixa de ler configuração do disco.** `initializationOptions` e `workspace/didChangeConfiguration` não são mais fontes de configuração — ter dois leitores do mesmo arquivo no mesmo binário deixava os dois livres para discordar.
- `Locale::from_str` passa a se chamar `from_tag`: infalível, e o nome anterior prometia o `Result` da trait padrão.
- A sondagem periódica (`server.ping`, `pidsOnPort`, `projectServersOnPort`, `engine.status`) e o próprio `log.write` deixam de entrar no registro quando dão certo: repetem-se a cada poucos segundos e afogavam o resto.
- **Licença própria**: a PawnPro-Core v1.0 (`LICENSE.md`) vale para as cinco crates. É um documento do núcleo, e não a licença da extensão: o texto foi reescrito sem as contradições que o anterior tinha, e o `LICENSE` antigo sai.
- **Engine — análise sobre o texto não salvo.** A coleta de includes e a verificação de símbolos não usados leem o que o editor tem aberto, e o disco só para o resto: uma `stock` nova num include aberto já vale no `.pwn` antes de salvar.
- **Engine — diagnósticos publicados com a versão do documento.** Texto e versão são lidos juntos, e o resultado de uma análise é descartado se uma edição chegou enquanto ela corria: avisos de um texto que já não existe não aparecem mais.
- **Engine — cache de identificadores por arquivo**, validado pela data de modificação e, para os abertos, pelo texto do editor. A análise do `molde.pwn` do projeto de teste, com 84 includes, caiu de 0,95 s para cerca de 0,2 s por edição. O contador de referências soma direto do cache, sem reler arquivos, e a busca de declarações lê os símbolos dele.
- **Engine — identificadores só ASCII nas expressões**, como no Pawn: o `\w` do Rust casa letras acentuadas.

### Corrigido

- Teste instável do supervisor: conferia o efeito do trabalho logo após a notificação de `running`, que é enviada antes de o trabalho começar.
- Teste instável de processos: esperava 300 ms fixos para um filho virar zumbi, prazo que a carga da suíte podia estourar.
- `${workspaceFolder}` não era substituído nos valores padrão da configuração, e `diagnostics.level` chegava vazio em vez de `off`.
- **Engine — a formatação danificava o código** em comentários no fim da linha e de bloco, em strings e caracteres com espaço, e em diretivas e macros com continuação `\`; e não era estável, mudando de novo o que já tinha formatado. Nos 270 arquivos do projeto de teste: nenhum dano, nenhuma instabilidade.
- **Engine — diretiva recuada**: o léxico só reconhecia `#` na coluna 0, e `    #include` virava pontuação solta.
- **Engine — `native`/`forward` com corpo na linha seguinte** não eram acusados.
- **Engine — pane na análise e na assinatura** com letras acentuadas: um nome cortado no limite de 31 bytes, e a linha cortada por byte antes do cursor.
- **Engine — referências e contador**: um nome dentro de uma string contava como referência; uma chamada numa declaração `new x = Nome();` era ignorada; a coluna ficava errada com acento antes do nome na linha.
- **Engine — renomear um parâmetro** alterava todo nome igual da unidade inteira.
- **Engine — `PP0012`** não disparava, e a verificação de não usados não via uma edição em outro arquivo.
- `native`/`forward` e `#include` deixam de ter `expect`/`unwrap` implícitos no parser.
- **Codificações**: os nomes da configuração `windows1250`, `windows1251` e `windows1253` a `windows1257` não são rótulos do `encoding_rs` e caíam no windows-1252; agora cada um chega à sua página de código.
- **Engine — títulos das correções rápidas** eram texto fixo em português; agora vêm das mensagens traduzidas.

### Removido

- `EngineConfig::load` e a leitura dos arquivos de lista; `SdkConfig`, que ninguém mais lia — o SDK chega resolvido.
- Métodos RPC sem uso: `engine.stop`, `engine.status`, `engine.reload`, `log.path`, `config.get`, `server.detectType` e `server.detectExecutable`.
- O módulo `ui/` do núcleo: temas, cor de destaque, idioma das páginas e cores são da extensão, que os desenha.
