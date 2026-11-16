# holo-files

Cliente OneDrive para Windows com **Files On-Demand**, feito para substituir o cliente
oficial sem travar o Explorer ao criar, renomear ou apagar arquivos.

- Placeholders nativos do Windows (Cloud Filter API): os arquivos aparecem no Explorer,
  são baixados só quando abertos e o Explorer ganha "Sempre manter neste dispositivo" e "Liberar espaço".
- **Callbacks de renomear/apagar são aprovados na hora** e enfileirados; a rede nunca bloqueia o Explorer.
  A listagem de pastas também não depende da rede (os placeholders são criados antes, a partir do delta).
- Hidratação parcial por *byte range*, com read-ahead configurável.
- Ignora lixo de desenvolvimento por padrão (`node_modules/`, `.git/`, `target/`, `dist/`...), nos dois sentidos.
- Arquivos abertos para escrita por outro programa (logs, bancos) esperam localmente, sem tocar a rede.
- Nunca sobrescreve em silêncio: conflitos viram `nome (conflict PC AAAA-MM-DD HHMMSS).ext`.
- Provedores atrás de um trait: OneDrive implementado; Google Drive com scaffold pronto.

## Arquitetura

```
crates/
  holo-core      modelo, trait CloudProvider, RelPath, ignore, nomes Windows, QuickXorHash
  holo-auth      OAuth 2.0 PKCE (loopback) genérico + tokens criptografados com DPAPI
  holo-state     SQLite: accounts, roots, items (base do 3-way), ops (journal), kv
  holo-onedrive  Microsoft Graph: delta, download ranged, upload simples/sessão, retry/429
  holo-gdrive    scaffold do Google Drive (OAuth pronto, métodos NotImplemented)
  holo-cfapi     Files On-Demand: registro do sync root, placeholders, callbacks
  holo-engine    motor de sync (agnóstico de provedor e de filesystem)
  holo-cli       binário `holo`
```

Fluxo:

```
remoto ─ changes() ─► plan (função pura) ─► aplica no LocalTree ─► SQLite
Explorer ─ callback CF ─► LocalEvent ─► debounce/classifica ─► journal (ops) ─► executor ─► remoto
leitura de placeholder ─ fetch_data ─► download do intervalo ─► escrita alinhada em 4 KiB
```

O motor só acessa o disco pelo trait `LocalTree`. Assim toda a lógica (plano, conflitos,
uploads, exclusões) é testada com um filesystem em memória e um provedor falso.

**Novo provedor** = implementar `holo_core::CloudProvider` e registrar em
`holo-cli/src/providers.rs`. As diferenças entre provedores ficam em `Capabilities`
(delta por pasta ou por drive, algoritmo de hash, nomes duplicados).

## Setup

1. **Registrar o app** (Azure Portal → Entra ID → Registros de aplicativo):
   - contas: *qualquer diretório organizacional + contas Microsoft pessoais*
   - plataforma: *Aplicativos móveis e da área de trabalho*, redirect `http://localhost`
   - *Permitir fluxos de cliente público*: **Sim**
   - permissões delegadas: `Files.ReadWrite`, `User.Read`, `offline_access`
2. Compilar: `cargo build --release` (binário em `target/release/holo.exe`).
3. `holo config` mostra onde fica o `config.toml`. Coloque o *Client ID* em `[onedrive].client_id`.
4. Usar:

```powershell
holo login                                  # abre o navegador
holo add-root D:\OneDrive\Teste --remote /holo-test --create   # --create: cria a pasta remota (e pais) se não existir
holo run                                    # foreground; Ctrl+C para parar
holo service install                        # roda em background no logon (tarefa agendada por usuário, sem janela)
holo service stop|start|status|uninstall     # stop = desligamento limpo
holo status                                 # --stream: acompanha ao vivo (itens, pending/held/failed com deltas)
holo ops                                    # journal; `release`/`discard` após o guard de exclusão em massa
holo ops --stream                           # acompanha o journal ao vivo: + nova, ~ mudou, - concluída/descartada
holo pin  D:\OneDrive\Teste\video.mp4       # sempre manter neste dispositivo
holo free D:\OneDrive\Teste\video.mp4       # liberar espaço
holo remove-root D:\OneDrive\Teste --yes
```

> Comece com uma **pasta de teste** (`--remote /holo-test`), não com o drive inteiro.

### Dry run (`holo plan`)

Compara nuvem × pasta local × último sync e lista o que seria feito, com a direção,
**sem alterar nada** (nem disco, nem banco, nem nuvem). Usa as mesmas funções de decisão do sync real.

```powershell
holo plan D:\Holo --remote /            # antes do add-root: preview do primeiro sync (cold start)
holo plan D:\Holo                       # root existente: mudanças desde o último sync
holo plan D:\Holo --full                # root existente: comparação completa
holo plan D:\Holo --no-hash             # não lê arquivos locais (mais rápido; marca "unverified")
holo plan D:\Holo --json plano.json --limit 0
```

Legenda: `↓` nuvem→PC, `↑` PC→nuvem, `⇄` conflito (os dois lados), `·` sem transferência agora
(placeholder, link, mover). O resumo mostra quanto trafega já e quanto fica disponível sob demanda.

> No Git Bash, use `MSYS_NO_PATHCONV=1` ou o PowerShell: o Bash converte `--remote /` em `C:/Program Files/Git/`.

### Ignorar arquivos (`.holoignore`)

Sintaxe do `.gitignore`. Vale em qualquer pasta do root: os padrões são relativos à pasta do
arquivo e o `.holoignore` mais fundo tem prioridade. `./x` é aceito como `/x` (só nesta pasta).

```gitignore
# Documents\Rockstar Games\Launcher\.holoignore
*.log
```

Criar, editar ou apagar um `.holoignore` vale na hora (o daemon recarrega) e tira da fila os
uploads pendentes que passaram a ser ignorados. Itens já enviados continuam na nuvem.

### Uploads em paralelo (`[sync].concurrency`)

```toml
[sync]
concurrency = "auto"   # ou um número fixo
concurrency_min = 1
concurrency_max = 16
```

No modo `auto` o número de uploads simultâneos é reavaliado a cada 2 s:

- **fila**: nunca mais workers que ops pendentes;
- **uso do PC**: sem teclado/mouse há 5 min → máximo; usando → metade; usando com CPU ≥ 75% → mínimo; na bateria → metade;
- **servidor**: HTTP 429/503 → corta pela metade e segura 30 s.

Sobe dobrando até o primeiro throttle e depois +1 por ciclo. `holo status --stream` mostra o valor atual e o motivo.

## Segurança dos dados

- Exclusões remotas vão para a lixeira do OneDrive; o conteúdo local hidratado de itens apagados
  remotamente vai para `%LOCALAPPDATA%\holo\data\trash`.
- Arquivo editado localmente e apagado no remoto é **reenviado como novo**, nunca descartado.
- Exclusão rechecada antes de executar (desfazer exclusão / mover não apaga no remoto).
- Mais de `mass_delete_threshold` exclusões por minuto ficam **retidas** até `holo ops release`.
- Upload com `If-Match` (eTag); se o remoto mudou, a versão local vira cópia de conflito.
- `USN` do arquivo protege contra marcar como sincronizado um arquivo editado durante o upload.

## Testes

```powershell
cargo test --workspace                                                     # unit + cenários do motor
cargo test -p holo-cfapi --test real_sync_root -- --ignored --nocapture    # driver Cloud Filter real
```

O teste do driver real registra um sync root temporário em `target/`, cria placeholders,
hidrata a partir de outro processo (PowerShell), valida os bytes, libera espaço, faz pin,
renomeia e apaga, e confere as notificações.

### Coverage

Mostra quais linhas os testes executam (verde) e quais nunca rodam (vermelho), por arquivo.
Usa [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov):

```powershell
rustup component add llvm-tools-preview    # uma vez
cargo install cargo-llvm-cov --locked      # uma vez

cargo llvm-cov --workspace --html --open   # relatório HTML (target/llvm-cov/html) no navegador
cargo llvm-cov --workspace --summary-only  # tabela no terminal
cargo llvm-cov --workspace --lcov --output-path lcov.info   # para o VS Code (Coverage Gutters)
```

O teste do driver real (`--ignored`) fica fora; para incluí-lo, adicione `-- --include-ignored`.

## Limitações conhecidas

- **Ainda não testado contra uma conta OneDrive real** (falta um Client ID): login, delta,
  upload e download do Graph são cobertos só por testes de parsing/unidade.
- Sessão de upload grande retoma dentro do processo, mas não depois de um crash (recomeça).
- Contas business usam delta da raiz do drive inteiro (o Graph não suporta delta por pasta lá).
- Sem tray, sem IPC e sem autostart: `holo run` roda em foreground.
- `holo add-root`/`plan` recusam pastas que já são sync root de outro provedor (ex.: OneDrive oficial).
- Itens compartilhados ("Adicionar ao meu OneDrive") e pacotes OneNote são ignorados.
- Hash não é validado após a hidratação (só tamanho/TLS).
