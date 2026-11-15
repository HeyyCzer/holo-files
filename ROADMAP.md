# Roadmap: cliente OneDrive otimizado (nome provisório: `fastdrive`)

## Objetivo

Cliente de sincronização bidirecional para OneDrive no **Windows**, construído sobre a **Microsoft Graph API oficial**, mais rápido e previsível que o cliente oficial para o uso de um desenvolvedor:

- sincroniza só **pastas selecionadas**;
- ignora lixo de desenvolvimento por padrão (`node_modules`, `.git`, `dist`, caches);
- concorrência configurável, sem overlay no Explorer nem integração pesada com o shell;
- baixo uso de CPU e disco em idle.

**Não é** engenharia reversa do cliente oficial. Tudo passa pela API pública.

## Fora do escopo do MVP

- Files On-Demand / placeholders (Cloud Filter API): fase futura, separada (ver o final do documento)
- Bibliotecas do SharePoint, pastas compartilhadas ("Adicionar ao meu OneDrive"), Cofre Pessoal
- Arquivos de pacote do OneNote
- Coautoria do Office
- Interface gráfica completa (o MVP é CLI + daemon)

## Stack

| Item | Escolha | Observação |
|---|---|---|
| Runtime | Node.js 22 LTS + TypeScript (strict) | Node por compatibilidade com addons nativos; avaliar Bun depois |
| Gerenciador | pnpm | |
| Auth | `@azure/msal-node` + `@azure/msal-node-extensions` | cache de token criptografado com DPAPI |
| HTTP | `undici` / fetch nativo | cliente Graph próprio, fino |
| Watcher | `@parcel/watcher` | usa `ReadDirectoryChangesW` nativo |
| Estado | `better-sqlite3` | modo WAL |
| Ignore | `ignore` | sintaxe estilo `.gitignore` |
| Logs | `pino` | JSON, com rotação |
| Testes | `vitest` | |
| CLI | `commander` ou `citty` | |

## Regras para quem implementar

1. **Nunca apagar arquivo do usuário de forma definitiva.** Exclusões locais vão para `.fastdrive-trash/` dentro da sync root (ou para a Lixeira do Windows). Exclusões remotas usam o delete normal do Graph, que manda para a lixeira do OneDrive.
2. Toda operação destrutiva tem **modo `--dry-run`**, e ele é o padrão até a Fase 6 estar validada.
3. Desenvolver e testar **sempre numa pasta de teste** (ex.: `/fastdrive-test` no OneDrive) ou numa conta de teste.
4. Cada fase termina com testes passando e com o critério de pronto atendido antes de começar a próxima.
5. Escrita local sempre **atômica**: grava em arquivo temporário na mesma pasta e depois renomeia.
6. Assumir Windows: caminhos com `\`, sistema de arquivos case-insensitive, suporte a caminho longo.

---

## Fase 0: Setup e registro do app

**Passo manual do usuário (Azure Portal, Entra ID > Registros de aplicativo):**
- Novo registro, contas: *qualquer diretório organizacional + contas Microsoft pessoais*
- Plataforma: *Aplicativos móveis e da área de trabalho*, redirect `http://localhost`
- Permitir fluxos de cliente público: **Sim**
- Permissões delegadas: `Files.ReadWrite`, `offline_access`, `User.Read`
- Anotar o **Client ID** (não há client secret, porque é um cliente público)

**Código:**
- Repositório com TS strict, ESLint, Prettier e vitest
- Estrutura sugerida:
  ```
  src/
    auth/        # MSAL, cache de token
    graph/       # cliente HTTP, retry, throttling
    state/       # SQLite, migrations
    remote/      # delta, download
    local/       # watcher, scan, ignore
    sync/        # reconciliação, fila de operações
    cli/
    daemon/
  ```
- Config em `%APPDATA%\fastdrive\config.json`; banco e logs em `%LOCALAPPDATA%\fastdrive\`

**Pronto quando:** `pnpm build` e `pnpm test` passam e a config é carregada e validada (zod).

## Fase 1: Autenticação e cliente Graph

- Login interativo (auth code + PKCE abrindo o navegador), com device code como alternativa
- Cache de token persistente e criptografado; refresh silencioso
- Cliente Graph com:
  - **limitador de concorrência** global (padrão 4, configurável)
  - tratamento de `429` e `503` respeitando `Retry-After`; backoff exponencial com jitter nos demais erros transitórios
  - timeouts e cancelamento (`AbortSignal`)
  - log de cada request com duração e status (sem tokens)
- CLI: `fastdrive login`, `fastdrive logout`, `fastdrive whoami`

**Pronto quando:** `whoami` mostra a conta e o espaço do drive (`GET /me/drive`), o token sobrevive a reinícios e o throttling simulado em teste é respeitado.

## Fase 2: Estado local (SQLite)

Schema inicial (ajustar se precisar):

```sql
sync_roots(id, local_path, remote_item_id, remote_path, enabled, created_at)
items(
  id INTEGER PK, root_id, remote_id, parent_remote_id,
  rel_path,            -- relativo à root, normalizado
  rel_path_lower,      -- para comparação case-insensitive
  is_dir, size, local_mtime, local_file_id,  -- file ID do NTFS (stat bigint ino)
  quickxor, etag, ctag, remote_mtime,
  last_synced_at, status  -- synced | pending_up | pending_down | conflict | error
)
kv(key PK, value)       -- deltaLink por root, versão do schema
op_journal(id, root_id, kind, payload_json, state, attempts, created_at, updated_at)
```

- Migrations versionadas
- `items` guarda o **último estado sincronizado** (a "base" da comparação de três vias da Fase 6)

**Pronto quando:** as migrations rodam do zero e de forma incremental, com testes de CRUD.

## Fase 3: Leitura remota (delta + download)

- Listagem inicial de uma sync root via `GET /me/drive/items/{id}/delta`, paginando com `@odata.nextLink` e guardando o `@odata.deltaLink` no `kv`
- Execuções seguintes usam só o `deltaLink`, nunca uma varredura completa
- Tratar `410 Gone` / `resyncRequired`: refazer a enumeração e reconciliar com o estado existente, sem apagar nada
- Download por streaming para um arquivo temporário, depois renomear; verificar o `quickXorHash` (implementar o algoritmo conforme a documentação da Microsoft, ou usar um pacote testado)
- Preservar o `lastModifiedDateTime` remoto como mtime local
- Validar e mapear nomes: avisar sobre nomes inválidos no Windows e sobre colisões só de maiúsculas e minúsculas

**Pronto quando:** `fastdrive pull <pasta-remota> <pasta-local>` espelha corretamente uma pasta com mais de 1.000 arquivos, e uma segunda execução sem mudanças termina em poucos segundos, fazendo só a chamada de delta.

## Fase 4: Sync unidirecional para baixo (remoto → local)

- Aplicar criações, alterações, renomeações e movimentos (pelo `remote_id`) e exclusões (mandando para a lixeira local)
- Fila de operações persistida em `op_journal`, que pode ser retomada após crash
- Polling do delta em intervalo configurável (padrão 30 s)

**Pronto quando:** mudanças feitas no site do OneDrive aparecem localmente; matar o processo no meio e reiniciar não deixa arquivo corrompido nem duplicado.

## Fase 5: Mudanças locais e upload

- `@parcel/watcher` na sync root, com **debounce** e agrupamento de eventos (padrão 1–2 s)
- **Scan de inicialização**: comparar o disco com `items` (mtime, tamanho e, se houver dúvida, hash) para pegar o que mudou com o app fechado
- Regras de ignore: padrões embutidos mais um `.fastdriveignore` por root
  - padrão: `node_modules/`, `.git/`, `dist/`, `build/`, `.next/`, `.turbo/`, `*.tmp`, `~$*`, `Thumbs.db`, `desktop.ini`
- Detectar arquivo ainda sendo escrito (aguardar tamanho/mtime estabilizar ou o lock ser liberado)
- Upload:
  - pequeno (≤ 4 MiB, configurável): `PUT /items/{parent-id}:/{nome}:/content`
  - grande: `createUploadSession`, chunks **múltiplos de 320 KiB** (padrão 10 MiB, nunca acima de 60 MiB), com retomada de sessão
  - usar `@microsoft.graph.conflictBehavior` explícito, e enviar `If-Match` com o eTag quando for atualização
- Criação de pastas antes dos filhos; renomear e mover por `PATCH` em vez de reenviar

**Pronto quando:** `fastdrive push` envia uma pasta grande respeitando o ignore; editar, renomear e mover localmente aparece corretamente no OneDrive; um upload de 1 GB interrompido continua de onde parou.

## Fase 6: Reconciliação bidirecional e conflitos

Comparação de três vias por item: **local atual × remoto atual × base (último sincronizado)**.

| Local | Remoto | Ação |
|---|---|---|
| mudou | igual à base | upload |
| igual à base | mudou | download |
| mudou | mudou (hash igual) | só atualizar a base |
| mudou | mudou (hash diferente) | **conflito** |
| apagado | igual à base | apagar no remoto |
| igual à base | apagado | mover o local para a lixeira |
| apagado | mudou | restaurar a partir do remoto |
| mudou | apagado | reenviar como novo |

- **Conflito:** manter as duas versões; a local é renomeada para `nome (conflito PC AAAA-MM-DD HHmm).ext` e enviada. Nunca sobrescrever em silêncio.
- **Renomear/mover local:** detectar pelo file ID do NTFS (`fs.stat(p, { bigint: true }).ino`) para não virar "apagou + criou"
- **Padrão de salvamento do Office e de editores** (grava no temporário e renomeia por cima): tratar como edição do arquivo original
- Pasta apagada de um lado com edição do outro lado dentro dela: preservar os arquivos editados
- Bateria de testes de cenário cobrindo cada linha da tabela, com fakes do Graph

**Pronto quando:** todos os cenários da tabela têm teste; uma semana de uso real numa pasta de teste sem perda de dados; `--dry-run` desligado por padrão.

## Fase 7: Robustez

- Recuperação de crash via `op_journal` (idempotência de cada operação)
- Modo offline: enfileirar e sincronizar quando a rede voltar
- Limites e regras do OneDrive: caracteres proibidos, nomes reservados (`CON`, `PRN`, `AUX`, `NUL`, `COM1`…), tamanho de caminho, arquivos de 0 byte
- Caminhos longos no Windows (prefixo `\\?\` quando preciso)
- Arquivos travados por outro processo: tentar de novo depois, sem quebrar a fila
- Proteção contra exclusão em massa: se uma única rodada for apagar mais de N arquivos ou X% da root, **pausar e pedir confirmação**
- Métricas: arquivos/s, bytes/s, tamanho da fila, contagem de 429

## Fase 8: Daemon e experiência de uso

- Processo em background com lock de instância única
- IPC local (named pipe) entre CLI e daemon
- CLI: `add-root`, `remove-root`, `status`, `pause`, `resume`, `sync-now`, `conflicts`, `logs`
- Inicialização automática: tarefa no Agendador de Tarefas ou chave `HKCU\...\Run`
- Ícone na bandeja (opcional; avaliar Tauri numa fase futura)
- Notificação do Windows para conflitos e erros

## Fase 9: Empacotamento e benchmark

- Executável único (Node SEA ou similar), incluindo os addons nativos
- Benchmark contra o cliente oficial com os mesmos cenários:
  1. upload inicial de 10 mil arquivos pequenos
  2. upload de 1 arquivo de 5 GB
  3. detecção de 1 alteração com o app ocioso (latência)
  4. CPU e RAM em idle por 10 minutos
- Documentar os resultados no README

---

## Fase futura: Files On-Demand

Só depois das fases 0–7 estarem sólidas. Projeto separado, em **Rust** (crate `cloud-filter`) ou C++, reaproveitando o estado, o delta e a lógica de conflitos:

- `CfRegisterSyncRoot` e criação de placeholders
- callbacks `FETCH_DATA` (hidratação, inclusive por intervalo de bytes) e `FETCH_PLACEHOLDERS` (enumeração sob demanda)
- notificações síncronas de renomear e apagar
- pin states ("sempre manter" / "liberar espaço")
- bloqueio de tempestades de hidratação (indexador, antivírus, miniaturas)
- integração de status no Explorer (`StorageProviderSyncRootManager`, pode exigir MSIX)

Riscos: travar processos de terceiros se um callback demorar, bugs difíceis de reproduzir e testes que exigem VM.

## Referências

- Microsoft Graph: OneDrive (driveItem, delta, upload session, throttling): https://learn.microsoft.com/graph/api/resources/onedrive
- quickXorHash: https://learn.microsoft.com/onedrive/developer/code-snippets/quickxorhash
- Restrições de nomes no OneDrive: https://support.microsoft.com/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa
- Cloud Filter API: https://learn.microsoft.com/windows/win32/cfapi/build-a-cloud-file-sync-engine
