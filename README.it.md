<div align="center">

<img src="docs/logo.png" width="120" alt="Logo Archivist">

# Archivist

*Fai il backup del tuo vault Obsidian. Ripristinalo byte per byte.*  
*Incrementale, deduplicato, lossless. Cifratura opzionale.*  

[English](README.md) · **Italiano** · [Sito](https://archivist.gusions.com) · [Download](https://github.com/Gusions/archivist/releases/latest)

[![Versione](https://img.shields.io/badge/version-0.1.0-0A66E8)](Cargo.toml)
[![Lingue](https://img.shields.io/badge/languages-2-0A66E8)](#aggiungere-una-lingua)
[![Piattaforme](https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-0A66E8)](#piattaforme)
[![CI](https://github.com/Gusions/archivist/actions/workflows/ci.yml/badge.svg)](https://github.com/Gusions/archivist/actions/workflows/ci.yml)
[![Build](https://github.com/Gusions/archivist/actions/workflows/release.yml/badge.svg)](https://github.com/Gusions/archivist/actions/workflows/release.yml)
[![Licenza: MIT](https://img.shields.io/badge/license-MIT-0A66E8)](LICENSE)
[![Stelle](https://img.shields.io/github/stars/Gusions/archivist?style=flat&color=0A66E8)](https://github.com/Gusions/archivist/stargazers)

Snapshot, non copie · lossless per costruzione · cifratura opzionale · verifica integrità inclusa · niente cloud, niente account · traducibile

</div>

**Un vault cresce per anni. Archivist conserva ogni backup come snapshot completo, scrive solo ciò che è cambiato e non altera mai i tuoi file.**

| Cosa c'è nel vault | Cosa fa Archivist | Perdita di qualità |
|---|---|---|
| Note, testo, JSON, CSS | Deduplica a blocchi + compressione zstd | Nessuna |
| PNG, JPEG, WebP, PDF, audio, video | Salvati così come sono (già compressi) | Nessuna |
| Nota modificata (poche righe) | Scrive solo i blocchi cambiati, pochi KB | Nessuna |
| File spostato o duplicato | Zero spazio in più | Nessuna |

<p align="center"><img src="docs/screenshots/overview-light.png" width="780" alt="Archivist, panoramica"></p>

## Per iniziare

```bash
cargo build --release --features window   # richiede Rust: https://rustup.rs
./target/release/archivist                # apre la finestra nativa
```

Nella finestra: scegli il vault (i vault di Obsidian vengono rilevati da soli), scegli una cartella di backup (meglio su un altro disco), premi **Esegui backup**. Tutta la configurazione è qui.

Preferisci un binario pronto? Scaricalo dalle [Release](../../releases). Su macOS `./make-dmg.sh` crea `dist/Archivist.app` e `dist/Archivist.dmg`.

> Se Gatekeeper blocca l'app al primo avvio: `xattr -d com.apple.quarantine <percorso>`

Poi, quando vuoi, sempre dalle **Impostazioni**: cifrare l'archivio, attivare i backup che partono anche ad app chiusa e sfogliare uno snapshot per ripristinare una sola nota. Dettagli più sotto.

### Piattaforme

| Piattaforma | Finestra | Backup in background | Portachiavi | Stato |
|---|---|---|---|---|
| macOS (arm64, x64) | WebKit (di sistema) | Sì (agente launchd) | Sì | Compilato e testato in unità dalla CI; app, agente launchd e Portachiavi provati su macOS arm64 |
| Windows x64 | WebView2 (preinstallato su Win 10/11) | Manuale (`schtasks`) | Non ancora | Compilato e testato in unità dalla CI |
| Linux x64 | WebKitGTK | Manuale (timer systemd o cron) | Non ancora | Compilato e testato in unità dalla CI |

Nessun browser incluso: il binario pesa pochi MB.

## Cosa fa

**Backup**
- **Snapshot incrementali**: ogni backup è una fotografia completa del vault.
- **Deduplica a blocchi** (FastCDC + BLAKE3) e **compressione lossless** (zstd) dove serve, nessuna dove non serve.
- **Retention**: ultimi N più giornalieri, settimanali, mensili. L'ultimo snapshot non viene mai eliminato.
- **Backup in background** su macOS, anche con l'app chiusa.

**Sicurezza**
- **Cifratura opzionale**: XChaCha20-Poly1305 con password Argon2id. Contenuti, nomi dei file, struttura delle cartelle e percorso del vault sono cifrati.
- **Verifica integrità**: rapida (presenza di tutti i blocchi) o completa (rilegge e rifà l'hash di ogni blocco).
- **Scritture durevoli**: i file si scrivono con un nome temporaneo, si forzano su disco e si rinominano in modo atomico; uno snapshot compare solo a scrittura completata.
- **Password nel Portachiavi di macOS** (opzionale), così i backup in background e l'avvio dell'app non la chiedono.

**Ripristino**
- **Ripristino esatto**: contenuto, date di modifica, cartelle vuote.
- **Sfoglia uno snapshot**: cerca per nome, vedi cosa è stato aggiunto, modificato o rimosso rispetto allo snapshot precedente, ripristina un file o una cartella (anche uno cancellato in seguito).
- **Export** di uno snapshot in un unico `.tar.zst` portabile.

**Ogni giorno**
- **Cifrare un archivio esistente**: copia tutti gli snapshot in un nuovo archivio cifrato, l'originale resta intatto.
- **Interfaccia in stile macOS** con tema chiaro, scuro o di sistema; **traducibile** (inglese e italiano inclusi).

## Come funziona

```
file del vault
   |  tagliati in blocchi di ~64 KB (FastCDC, content-defined)
   v
hash BLAKE3 per blocco ---- già salvato? ---- sì --> solo riferimento
   |  no
   v
compressione zstd (saltata per i tipi già compressi)
   v
cifratura (solo archivi cifrati)
   v
aggiunto a un pack (~64 MB)  -->  snapshot scritto per ultimo = visibile solo se completo
```

La logica dell'archivio parla con una piccola interfaccia di archiviazione (`src/backend.rs`), così dove stanno i byte è separato da cosa significano. Oggi l'unica implementazione è una cartella locale (un disco esterno, un volume di rete); le destinazioni remote sono in programma.

## Comandi

| Comando | Cosa fa |
|---|---|
| `archivist backup` | Crea uno snapshot |
| `archivist list` / `stats` | Elenca gli snapshot / mostra dimensioni e rapporto |
| `archivist verify [--full]` | Controlla l'integrità |
| `archivist restore <id\|latest> <cartella> [--path P]...` | Ripristina in una cartella vuota o nuova; `--path` ripristina solo quei file o cartelle |
| `archivist files <id\|latest> [--grep TESTO]` | Elenca i file di uno snapshot |
| `archivist diff [vecchio] [nuovo]` | Mostra file aggiunti, modificati e rimossi tra due snapshot |
| `archivist export <id\|latest> <file>` | Esporta in `.tar.zst` |
| `archivist prune` | Applica la retention |
| `archivist forget <id>` | Elimina uno snapshot e recupera lo spazio non più referenziato |
| `archivist init --encrypt` | Crea un archivio cifrato (chiede una password) |
| `archivist copy <dest> [--encrypt]` | Copia tutti gli snapshot in un altro archivio, ricodificando ogni blocco. Con `--encrypt` crea un archivio cifrato da uno in chiaro; ricopiando si aggiungono solo gli snapshot nuovi |
| `archivist passwd` | Cambia la password (i dati non vengono riscritti) |
| `archivist keychain save\|forget` | Ricorda o dimentica la password nel Portachiavi di macOS |
| `archivist schedule install [--every MIN]\|remove\|status` | Backup in background (macOS) |
| `archivist ui` | Interfaccia web locale nel browser invece della finestra |
| `archivist window` | La finestra nativa (serve una build con `--features window`) |

```bash
archivist --repo /Volumes/Disco/ObsidianBackup backup --vault ~/Documents/MioVault
archivist restore latest ~/Desktop/Ripristino
archivist restore latest ~/Desktop/Una --path Notes/idea.md
```

Le impostazioni (vault, cartella dell'archivio, esclusioni, retention, tema, lingua) si salvano da UI in `~/Library/Application Support/Archivist/settings.json` (macOS) o `%APPDATA%\Archivist\settings.json` (Windows); la CLI le usa come default.

### Variabili d'ambiente

| Variabile | A cosa serve |
|---|---|
| `ARCHIVIST_PASSWORD` | Password dell'archivio (e nuova password di `init --encrypt`), al posto del prompt |
| `ARCHIVIST_PASSWORD_FILE` | Un file la cui prima riga è la password |
| `ARCHIVIST_NEW_PASSWORD` | La nuova password di `passwd` e `copy --encrypt` negli script |
| `ARCHIVIST_CONFIG_DIR` | Un'altra cartella delle impostazioni (profili separati); un agente in background installato con questa variabile continua a usarla |

Da dove arriva la password, in ordine: impostata nella sessione dell'app, `ARCHIVIST_PASSWORD`, `ARCHIVIST_PASSWORD_FILE`, Portachiavi di macOS, poi il prompt.

## Cifratura

- **Crea** un archivio cifrato da Impostazioni, Cifratura (oppure `archivist init --encrypt`). Solo per archivi nuovi; nell'app la password deve avere almeno 8 caratteri.
- **Cosa è protetto**: contenuto dei file, nomi dei file, struttura delle cartelle, percorso del vault, metadati degli snapshot. Tutto è cifrato e autenticato (XChaCha20-Poly1305); la chiave deriva dalla password con Argon2id. Cambiare la password riscrive un solo piccolo file di chiavi, i dati non si toccano. Un archivio cifrato rifiuta blocchi non cifrati.
- **Cosa non è nascosto**: il numero degli snapshot, le loro date e la dimensione dei blocchi salvati.
- **Se perdi la password i dati non sono recuperabili.** Non esiste un ripristino.
- **Cifrare un archivio esistente**: Impostazioni > Cifratura > "Crea copia cifrata", oppure `archivist copy <nuova-cartella> --encrypt`. Tutti gli snapshot vengono copiati in un nuovo archivio cifrato; il vecchio archivio non viene modificato né eliminato, toglilo tu quando sei sicuro.
- Le versioni precedenti di Archivist rifiutano di aprire gli archivi cifrati invece di leggerli male.
- È uno schema nuovo costruito su librerie note (RustCrypto). Non è stato verificato da terzi.

## Backup in background

- **macOS**: `archivist schedule install --every 60` (oppure Impostazioni, Backup, "Backup in background") installa un agente launchd dell'utente che esegue `archivist backup` ogni N minuti, anche con l'app chiusa. Log: `~/Library/Logs/Archivist/backup.log`. `archivist schedule status` mostra se è attivo e l'ultima riga del log; `archivist schedule remove` lo disattiva. L'agente punta al programma che l'ha installato: se sposti o elimini l'app, reinstallalo. La prima volta macOS può chiedere di consentire l'accesso alla cartella del vault (Documenti, iCloud Drive…); se non lo chiede, il log mostrerà un errore di permessi.
- **Archivi cifrati**: serve la password nel Portachiavi: `archivist keychain save` oppure spunta "Ricorda nel Portachiavi" quando crei o sblocchi l'archivio. `archivist keychain forget` la rimuove.
- **Windows**: `schtasks /Create /SC HOURLY /TN "Archivist" /TR "C:\percorso\archivist.exe backup"`. **Linux**: un timer systemd utente o cron che esegue `archivist backup`. Non ancora automatizzati; per gli archivi cifrati usa `ARCHIVIST_PASSWORD_FILE`.
- Il backup automatico dentro l'app (Impostazioni, Backup) funziona solo ad app aperta.

## Garanzie

- Uno snapshot compare solo a scrittura completata e forzata su disco. Un backup interrotto lascia al massimo dati orfani, ripuliti dalla prossima `prune`. I backup precedenti non vengono mai modificati.
- Un solo scrittore alla volta: l'archivio è bloccato mentre girano backup, prune o copia. Il lock è tenuto vivo da un heartbeat, quindi un backup lungo non viene scambiato per uno andato in crash, e un lock abbandonato scade dopo due minuti.
- L'UI ascolta solo su `127.0.0.1`, richiede un token casuale per ogni chiamata e rifiuta Host non locali (anti DNS-rebinding).
- Il ripristino rifiuta percorsi con `..` e cartelle di destinazione non vuote.
- I danni vengono rilevati: `verify --full` rifà l'hash di ogni blocco. In un archivio cifrato ogni blocco è anche autenticato quando viene letto, quindi un blocco alterato non può essere ripristinato.

## Non ancora

- Destinazioni remote (S3, SFTP, WebDAV): l'interfaccia di archiviazione è pronta, manca l'implementazione.
- Backup in background e Portachiavi su Windows e Linux.
- Conversione di un archivio sul posto (oggi: copia in un archivio nuovo).
- Albero delle cartelle e anteprima o diff del testo quando si sfoglia uno snapshot (oggi: ricerca ed elenco delle modifiche).
- Link simbolici, permessi Unix e attributi estesi.

## Aggiungere una lingua

Copia `i18n/en.json` in `i18n/xx.json`, traduci i valori (mantieni i `{segnaposto}`) e aggiungi una riga a `CATALOGS` in `src/i18n.rs`. Le chiavi mancanti ricadono sull'inglese.

## Sviluppo

```bash
cargo test                                  # test del motore (eseguiti su Linux, macOS e Windows dalla CI)
cargo build --release --features window     # build con finestra nativa
```

Struttura: `src/repo.rs` (formato dell'archivio), `src/backend.rs` (interfaccia di archiviazione e cartella locale), `src/crypto.rs` (cifratura), `src/backup.rs` (blocchi e snapshot), `src/ops.rs` (ripristino, diff, export, verifica, retention), `src/copy.rs` (copia tra archivi), `src/schedule.rs` e `src/keychain.rs` (agente in background e Portachiavi su macOS), `src/server.rs` (API locale), `src/window.rs` (finestra nativa e menu), `ui/index.html` (interfaccia), `i18n/` (testi).

Per i test che non devono toccare il sistema reale, `ARCHIVIST_SCHEDULE_LABEL` e `ARCHIVIST_KEYCHAIN_SERVICE` rinominano l'agente launchd e la voce del Portachiavi.

Tutte le stringhe (interfaccia, motore, CLI, menu) stanno in `i18n/<codice>.json`. Lingua e tema (Sistema / Chiaro / Scuro) si scelgono da Impostazioni → Generale; il tema si cambia anche con il pulsante nella barra. Il menu nativo di macOS usa la lingua al successivo avvio.

## FAQ

**Sostituisce Time Machine o la sincronizzazione cloud?**
No. Protegge una cosa sola, un vault Obsidian, con storico e controlli di integrità. Tieni gli altri backup.

**Rimpicciolisce le mie immagini?**
No, ed è voluto. Foto e PDF sono già compressi, quindi vengono salvati identici. Risparmi spazio su testo, contenuti ripetuti e storico, non sui media.

**Ho cancellato una nota la settimana scorsa. Come la recupero?**
Snapshot, Sfoglia su uno snapshot che la conteneva ancora, cerca il nome, Ripristina. Oppure `archivist files latest --grep nome` e `archivist restore <id> <cartella> --path <file>`.

**Posso riavere i dati senza Archivist?**
Sì, in due modi: `archivist export` scrive un `.tar.zst` normale di qualsiasi snapshot. Un archivio in chiaro è inoltre solo pack di blocchi compressi con zstd più JSON; uno cifrato richiede la password e Archivist.

**Fa il backup anche con l'app chiusa?**
Su macOS sì, se attivi "Backup in background". Su Windows e Linux usa lo scheduler di sistema (vedi sopra).

**Dove viene conservata la mia password?**
Mai in un file. L'app la tiene in memoria per la sessione. Su macOS le finestre di creazione e sblocco hanno "Ricorda nel Portachiavi" spuntato di default: la password va nel Portachiavi del tuo login, così avvii e backup in background non la chiedono; togli la spunta per tenerla solo in memoria.

**Cosa non viene conservato?**
I link simbolici vengono ignorati; permessi Unix e attributi estesi non vengono salvati (le date di modifica sì).

**Due computer possono scrivere sullo stesso archivio?**
Non contemporaneamente. Il lock è locale, non di rete.

**Vault con `.git`?**
Viene incluso come ogni altra cartella. Aggiungi `.git` alle esclusioni se non lo vuoi.

## Contribuire

Issue e pull request sono benvenute, soprattutto traduzioni e prove sulle piattaforme (Windows e Linux sono coperti solo dalla CI).

## Licenza

[MIT](LICENSE)
