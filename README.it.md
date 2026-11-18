<div align="center">

<img src="docs/logo.png" width="112" alt="Logo Archivist">

# Archivist

**Backup incrementali e lossless del tuo vault Obsidian.**<br>
Ripristinalo byte per byte. Niente cloud, niente account.

[Download](https://github.com/Gusions/archivist/releases/latest) &nbsp;·&nbsp; [Sito](https://archivist.gusions.com) &nbsp;·&nbsp; [Avvio rapido](#avvio-rapido) &nbsp;·&nbsp; [Riga di comando](#riga-di-comando) &nbsp;·&nbsp; [FAQ](#faq) &nbsp;·&nbsp; [English](README.md) | **Italiano**

[![Release](https://img.shields.io/github/v/release/Gusions/archivist?style=flat-square&color=0A66E8&label=release)](https://github.com/Gusions/archivist/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/Gusions/archivist/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/Gusions/archivist/actions/workflows/ci.yml)
[![Piattaforme](https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-0A66E8?style=flat-square)](#piattaforme)
[![Licenza: MIT](https://img.shields.io/badge/licenza-MIT-0A66E8?style=flat-square)](LICENSE)

<br>

<img src="docs/screenshots/overview-light.png" width="780" alt="Archivist, panoramica">

</div>

<br>

Un vault cresce per anni. Archivist conserva ogni backup come **snapshot completo**, scrive **solo ciò che è cambiato** e **non altera mai i tuoi file**.

| Nel vault | Cosa fa Archivist | Perdita di qualità |
|---|---|:---:|
| Note, testo, JSON, CSS | Deduplica a blocchi + compressione zstd | Nessuna |
| PNG, JPEG, WebP, PDF, audio, video | Salvati così come sono (già compressi) | Nessuna |
| Una nota modificata (poche righe) | Scrive solo i blocchi cambiati, pochi KB | Nessuna |
| Un file spostato o duplicato | Zero spazio in più | Nessuna |

## Indice

[Avvio rapido](#avvio-rapido) · [Funzioni](#funzioni) · [Come funziona](#come-funziona) · [Riga di comando](#riga-di-comando) · [Cifratura](#cifratura) · [Backup in background](#backup-in-background) · [Garanzie](#garanzie) · [Configurazione](#configurazione) · [Aggiungere una lingua](#aggiungere-una-lingua) · [Sviluppo](#sviluppo) · [FAQ](#faq)

## Avvio rapido

**Scarica.** Prendi l'ultima versione da [Releases](../../releases): `Archivist-macOS-arm64.dmg` per Apple Silicon, più zip per macOS, Windows e Linux.

**Oppure compila dai sorgenti** (serve [Rust](https://rustup.rs)):

```bash
cargo build --release --features window
./target/release/archivist                # apre la finestra nativa
```

Nella finestra scegli il vault (i vault Obsidian vengono rilevati da soli), scegli una cartella di backup (meglio su un altro disco) e premi **Esegui backup**. Fine della configurazione.

> [!NOTE]
> L'app per macOS non è notarizzata. Se Gatekeeper la blocca al primo avvio, esegui `xattr -d com.apple.quarantine <percorso-app>`. Per creare il `.dmg` da solo, esegui `./make-dmg.sh` (produce `dist/Archivist.app` e `dist/Archivist.dmg`).

### Piattaforme

| Piattaforma | Finestra | Stato |
|---|---|---|
| macOS (arm64, x64) | WebKit (di sistema) | Compilato e testato da CI; `.dmg` arm64 in ogni release |
| Windows x64 | WebView2 (preinstallato su Win 10/11) | Compilato e testato da CI |
| Linux x64 | WebKitGTK | Compilato ed eseguito in CI e in sviluppo |

Nessun browser incluso: il binario pesa pochi MB.

## Funzioni

| | |
|---|---|
| **Snapshot** | Ogni backup è una fotografia completa del vault, salvata in modo incrementale |
| **Deduplica** | A blocchi (FastCDC + BLAKE3) |
| **Compressione** | zstd lossless dove serve, nessuna dove non serve |
| **Ripristino esatto** | Contenuto, date di modifica, cartelle vuote |
| **Cifratura** | Opzionale, XChaCha20-Poly1305 con passphrase Argon2id; cifrati anche nomi e percorsi |
| **Verifica integrità** | Rapida (tutti i blocchi presenti) o completa (rilegge e rifà l'hash di ogni blocco) |
| **Retention** | Ultimi N più giornalieri, settimanali, mensili; lo snapshot più recente non viene mai eliminato |
| **Singoli file** | Cerca in uno snapshot, vedi cosa è cambiato rispetto al precedente, ripristina un file o una cartella |
| **Export** | Qualsiasi snapshot in un `.tar.zst` portabile |
| **Backup in background** | Agente launchd su macOS, password nel Portachiavi |
| **Interfaccia nativa** | Finestra in stile macOS con tema chiaro, scuro e di sistema |
| **Traducibile** | Ogni stringa sta in `i18n/<codice>.json` (inglese e italiano inclusi) |

## Come funziona

```
file del vault
   │  tagliati in blocchi da ~64 KB (FastCDC)
   ▼
hash BLAKE3 per blocco ──── già salvato? ──── sì ──▶ solo riferimento
   │ no
   ▼
compressione zstd (saltata per i tipi incomprimibili)
   ▼
aggiunto a un pack (~64 MB)  ──▶  snapshot scritto per ultimo = visibile solo se completo
```

## Riga di comando

```bash
archivist --repo /Volumes/Disco/ObsidianBackup backup --vault ~/Documents/MioVault
archivist restore latest ~/Desktop/Ripristino
```

**Snapshot**

| Comando | Cosa fa |
|---|---|
| `archivist backup` | Crea uno snapshot |
| `archivist list` / `stats` | Elenca gli snapshot / mostra dimensioni e rapporto |
| `archivist files <id\|latest> [--grep TESTO]` | Elenca i file di uno snapshot |
| `archivist diff [vecchio] [nuovo]` | File aggiunti, modificati e rimossi fra due snapshot |
| `archivist restore <id\|latest> <cartella> [--path P]...` | Ripristina in una cartella vuota o nuova; `--path` ripristina solo quei file o cartelle |
| `archivist export <id\|latest> <file>` | Esporta come `.tar.zst` |

**Manutenzione**

| Comando | Cosa fa |
|---|---|
| `archivist verify [--full]` | Controlla l'integrità |
| `archivist prune` | Applica la retention |
| `archivist forget <id>` | Elimina uno snapshot e libera lo spazio non referenziato |

**Archivio e app**

| Comando | Cosa fa |
|---|---|
| `archivist init --encrypt` | Crea un archivio cifrato (chiede una password) |
| `archivist copy <dest> [--encrypt]` | Copia tutti gli snapshot in un altro archivio, ricodificando ogni blocco. Con `--encrypt` crea un archivio cifrato da uno in chiaro; ricopiando si aggiungono solo i nuovi snapshot |
| `archivist passwd` | Cambia la password (i dati non vengono riscritti) |
| `archivist schedule install\|status\|remove` | Backup in background (macOS) |
| `archivist keychain save\|forget` | Ricorda o dimentica la password dell'archivio nel Portachiavi (macOS) |
| `archivist window` | Apre la finestra nativa (build con `--features window`) |
| `archivist ui` | Interfaccia web locale nel browser invece della finestra |

## Cifratura

Crea un archivio cifrato con `archivist init --encrypt`, oppure dall'app (Impostazioni, Cifratura). Contenuti, nomi dei file, struttura delle cartelle e percorso del vault sono cifrati e autenticati (XChaCha20-Poly1305, chiave derivata dalla password con Argon2id). Non sono nascosti: numero e date degli snapshot e dimensioni dei blocchi.

> [!WARNING]
> Se perdi la password i dati non sono recuperabili. Le versioni precedenti di Archivist rifiutano di aprire gli archivi cifrati.

Per cifrare un archivio in chiaro esistente usa Impostazioni, Cifratura, "Crea copia cifrata" oppure `archivist copy <nuova-cartella> --encrypt`: tutti gli snapshot vengono copiati in un nuovo archivio cifrato e l'originale resta intatto. Negli script imposta `ARCHIVIST_PASSWORD` o `ARCHIVIST_PASSWORD_FILE`; altrimenti la CLI la chiede.

## Backup in background

| Piattaforma | Come |
|---|---|
| **macOS** | `archivist schedule install --every 60` (oppure Impostazioni, Backup, "Backup in background") installa un agente launchd dell'utente che esegue `archivist backup` ogni N minuti, anche con l'app chiusa |
| **Windows** | `schtasks /Create /SC HOURLY /TN "Archivist" /TR "C:\percorso\archivist.exe backup"` (non ancora automatizzato) |
| **Linux** | Un timer systemd utente o cron che esegue `archivist backup` (non ancora automatizzato) |

- Log: `~/Library/Logs/Archivist/backup.log`. `archivist schedule status` mostra se è attivo e l'ultima riga del log; `archivist schedule remove` lo disattiva.
- L'agente punta al programma che l'ha installato: se sposti o elimini l'app, reinstallalo.
- La prima volta macOS può chiedere di consentire l'accesso alla cartella del vault (Documenti, iCloud Drive...); se non lo chiede, il log mostra un errore di permessi.
- **Archivi cifrati**: serve la password nel Portachiavi: `archivist keychain save`, oppure spunta "Ricorda nel Portachiavi" quando crei o sblocchi l'archivio. Su Windows e Linux usa `ARCHIVIST_PASSWORD_FILE`.
- Il backup automatico dentro l'app (Impostazioni, Backup) funziona solo ad app aperta.

## Garanzie

- Uno snapshot compare solo a scrittura completata. Un backup interrotto lascia al massimo dati orfani, ripuliti dalla prossima `prune`. I backup precedenti non vengono mai modificati.
- Ripristino ed export rifanno l'hash di ogni blocco letto: un archivio danneggiato dà un errore chiaro invece di produrre file sbagliati in silenzio.
- Le scritture nell'archivio sono durevoli e atomiche, con lock a heartbeat; test di crash e di andata e ritorno girano in CI su Linux, macOS e Windows.
- L'interfaccia locale ascolta solo su `127.0.0.1`, richiede un token casuale per ogni chiamata e rifiuta Host non locali (anti DNS-rebinding).
- Il ripristino rifiuta percorsi con `..` e cartelle di destinazione non vuote.

## Configurazione

Le impostazioni (vault, cartella archivio, esclusioni, retention, tema, lingua) si salvano dall'interfaccia in `~/Library/Application Support/Archivist/settings.json` (macOS) o `%APPDATA%\Archivist\settings.json` (Windows). La CLI le usa come default.

| Variabile | Scopo |
|---|---|
| `ARCHIVIST_PASSWORD` / `ARCHIVIST_PASSWORD_FILE` | Password dell'archivio per script e lavori in background |
| `ARCHIVIST_CONFIG_DIR` | Usa un'altra cartella delle impostazioni (profili separati); un agente installato con questa variabile continua a usarla |

## Aggiungere una lingua

Copia `i18n/en.json` in `i18n/xx.json`, traduci i valori (mantieni i `{segnaposto}`), poi aggiungi una riga a `CATALOGS` in `src/i18n.rs`. Le chiavi mancanti ricadono sull'inglese. Lingua e tema (Sistema / Chiaro / Scuro) si scelgono da Impostazioni, Generale.

## Sviluppo

```bash
cargo test                                  # test del motore
cargo build --release --features window     # build con finestra nativa
```

| File | Ruolo |
|---|---|
| `src/repo.rs` | Formato dell'archivio |
| `src/backend.rs` | Backend di storage |
| `src/crypto.rs` | Cifratura |
| `src/backup.rs` | Chunking e snapshot |
| `src/copy.rs` | Copia e cifratura di un archivio |
| `src/ops.rs` | Ripristino, export, verifica, retention |
| `src/schedule.rs`, `src/keychain.rs` | Backup in background, Portachiavi |
| `src/server.rs` | API locale |
| `src/window.rs` | Finestra nativa e menu |
| `ui/index.html`, `i18n/` | Interfaccia e stringhe |

## FAQ

<details>
<summary><b>Sostituisce Time Machine o la sincronizzazione cloud?</b></summary>

No. Protegge una cosa sola, un vault Obsidian, con storico e controlli di integrità. Tieni gli altri backup.
</details>

<details>
<summary><b>Riduce le mie immagini?</b></summary>

No, ed è voluto. Foto e PDF sono già compressi, quindi vengono salvati invariati. Risparmi spazio su testo, contenuti ripetuti e storico, non sui media.
</details>

<details>
<summary><b>Cosa non viene conservato?</b></summary>

I link simbolici vengono ignorati; permessi Unix e attributi estesi non sono salvati (le date di modifica sì).
</details>

<details>
<summary><b>Due computer possono scrivere sullo stesso archivio?</b></summary>

Non contemporaneamente. Il lock è locale, non di rete.
</details>

<details>
<summary><b>Vault con <code>.git</code>?</b></summary>

Viene incluso come ogni altra cartella. Aggiungi `.git` alle esclusioni se non lo vuoi.
</details>

## Contribuire

Issue e pull request sono benvenute, soprattutto traduzioni e test sulle piattaforme.

## Licenza

[MIT](LICENSE)
