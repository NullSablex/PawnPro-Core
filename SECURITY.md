# Política de Segurança — PawnPro Core

## Reportar uma vulnerabilidade

Encontrou uma vulnerabilidade? **Não abra uma issue pública.**

Reporte de forma privada por um destes canais:

- Abra um [Security Advisory](https://github.com/NullSablex/PawnPro-Core/security/advisories/new) privado no GitHub (preferido); ou
- Envie um e-mail diretamente ao mantenedor.

Inclua, se possível: uma descrição do problema, os passos para reproduzir, a versão afetada e o impacto esperado. Resposta inicial em até **7 dias úteis**.

---

## Escopo

Esta política cobre o núcleo `pawnpro-core` e as crates publicadas com ele: o motor de análise (`pawnpro-engine`), o adaptador de depuração (`pawnpro-dap-adapter`), o protocolo de depuração (`pawnpro-dbg-protocol`) e o plugin de depuração que roda dentro do servidor do jogo (`pawnpro-debug-plugin`).

O que **está** no escopo:

- Execução de código, escalonamento de privilégios ou vazamento de dados a partir do núcleo, do motor, do adaptador ou do plugin.
- O **soquete local** por onde passam o LSP, a depuração e o plugin: quem consegue conectar, o que a saudação aceita e o que cada canal expõe.
- Manuseio de credenciais — em especial a senha de RCON, que o protocolo do servidor trafega em texto claro.
- A **posse de processo**: qualquer caminho que permita encerrar um processo que não seja o servidor do projeto e do mesmo usuário.
- Tratamento inseguro de arquivos do projeto, da configuração (`.pawnpro/`), das listas `.ban`/`.allow`, do estado local e da entrada do compilador.
- Os workflows deste repositório e o conteúdo publicado nas releases.

O que **não** está no escopo:

- Vulnerabilidades em dependências de terceiros — reporte aos respectivos mantenedores. As bibliotecas compiladas nos binários estão listadas nos arquivos `*-THIRD-PARTY.txt` de cada release.
- Comportamento do compilador `pawncc`, do servidor SA-MP/open.mp ou de plugins de terceiros carregados por ele.
- A extensão do editor, que tem [política própria](https://github.com/NullSablex/PawnPro/blob/master/SECURITY.md).
- Configurações inseguras feitas pelo próprio usuário (por exemplo, expor a porta de RCON publicamente).
- O uso do depurador num servidor de produção: ele é para desenvolvimento local, e pausar a máquina virtual do Pawn é o comportamento esperado dele.

---

## Versões suportadas

Somente a versão mais recente recebe correções de segurança. Os binários são distribuídos nas [Releases do GitHub](https://github.com/NullSablex/PawnPro-Core/releases) e dentro do VSIX da extensão.

---

## Práticas do projeto

- As dependências de CI são fixadas por commit SHA, e as ferramentas instaladas no CI, por versão; o build usa o `Cargo.lock` versionado e as dependências das docs são instaladas com hash (`pip install --require-hashes`).
- **Análise estática** via CodeQL (`rust` e `actions`), **auditoria de dependências** via `cargo-audit` a cada push e PR, e avaliação de boas práticas via **OpenSSF Scorecard**.
- Atualizações de dependências chegam pelo **Dependabot**, nos três ecossistemas do repositório.
- As licenças das bibliotecas compiladas são geradas no release pelo `cargo-about`, e uma licença fora da lista aceita impede a publicação.
