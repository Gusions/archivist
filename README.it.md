<div align="center">

<img src="docs/logo.png" width="120" alt="Logo Archivist">

# Archivist

*Fai il backup del tuo vault Obsidian. Ripristinalo byte per byte.*  
*Incrementale, deduplicato, lossless. Un solo piccolo eseguibile Rust.*  
**English** · [Italiano](README.it.md)

[English](README.md) · **Italiano** · [Sito](https://archivist.gusions.com) · [Download](https://github.com/Gusions/archivist/releases/latest)

Snapshot, non copie · lossless per costruzione · verifica integrità inclusa · niente cloud, niente account · traducibile

</div>

**Un vault cresce per anni. Archivist conserva ogni backup come snapshot completo, scrive solo ciò che è cambiato e non altera mai i tuoi file.**

<p align="center"><img src="docs/screenshots/overview-light.png" width="780" alt="Archivist, panoramica"></p>

## Cosa fa

- **Snapshot incrementali**: ogni backup è una fotografia completa del vault, ma salva solo ciò che è cambiato.
- **Deduplica a blocchi** (FastCDC + BLAKE3): una nota modificata di poche righe costa pochi KB, non l'intero file. Anche i file spostati o duplicati non occupano spazio due volte.
- **Compressione zstd lossless** sui testi; immagini, PDF già compressi, video e audio non vengono ricompressi (zero perdita, zero tempo sprecato). **Mai** ricompressione con perdita.
- **Ripristino bit-a-bit**: contenuto, date di modifica e cartelle vuote.
- **Verifica integrità** rapida (presenza blocchi) o completa (rilegge e rifà l'hash di ogni blocco).
- **Retention** automatica: ultimi N + giornalieri/settimanali/mensili.
- **Export** di uno snapshot in un unico `.tar.zst` portabile.
- Rilevamento automatico dei vault dalla configurazione di Obsidian.

## Installazione

Scarica il binario dalle Actions/Release del repo (macOS arm64/x64, Windows x64), oppure compila:

```bash
cargo build --release      # richiede Rust: https://rustup.rs
./target/release/archivist ui
```

macOS: al primo avvio se Gatekeeper blocca il binario → `xattr -d com.apple.quarantine archivist`.

## App con finestra nativa (consigliata)

```bash
cargo build --release --features window
./target/release/archivist            # apre la finestra (senza argomenti)
```
Usa WebKit di sistema su macOS, WebView2 su Windows (preinstallato su Win 10/11), WebKitGTK su Linux: nessun browser, nessun Chromium incluso, binario di pochi MB.
Su macOS `./make-dmg.sh` crea `dist/Archivist.app` e `dist/Archivist.dmg`.

## Uso

```bash
archivist ui                          # (o doppio clic sull'eseguibile) apre l'interfaccia nel browser (127.0.0.1, protetta da token)
archivist --repo /Volumes/Disco/ObsidianBackup backup --vault ~/Documents/MioVault
archivist list
archivist stats
archivist verify --full
archivist restore latest ~/Desktop/Ripristino     # la cartella deve essere vuota o nuova
archivist restore latest ~/Desktop/Una --path Notes/idea.md   # solo un file o una cartella
archivist files latest --grep idea                # cerca tra i file di uno snapshot
archivist diff                                    # cosa è cambiato rispetto allo snapshot precedente
archivist export latest ~/Desktop/vault.tar.zst
archivist prune                                   # applica la retention delle impostazioni
```

Le impostazioni (vault, archivio, esclusioni, retention) si salvano da UI in
`~/Library/Application Support/Archivist/settings.json` (macOS) o `%APPDATA%\Archivist\settings.json` (Windows);
la CLI le usa come default.

## Backup in background

- **macOS**: `archivist schedule install --every 60` (oppure Impostazioni, Backup, "Backup in background") installa un agente launchd dell'utente che esegue `archivist backup` ogni N minuti, anche con l'app chiusa. Log: `~/Library/Logs/Archivist/backup.log`. `archivist schedule status` mostra se è attivo e l'ultima riga del log; `archivist schedule remove` lo disattiva. L'agente punta al programma che l'ha installato: se sposti o elimini l'app, reinstallalo. La prima volta macOS può chiedere di consentire l'accesso alla cartella del vault (Documenti, iCloud Drive…); se non lo chiede, il log mostrerà un errore di permessi.
- **Archivi cifrati**: serve la password nel Portachiavi: `archivist keychain save` oppure spunta "Ricorda nel Portachiavi" quando crei o sblocchi l'archivio. `archivist keychain forget` la rimuove.
- **Windows**: `schtasks /Create /SC HOURLY /TN "Archivist" /TR "C:\\percorso\\archivist.exe backup"`. **Linux**: un timer systemd utente o cron che esegue `archivist backup`. Non ancora automatizzati; per gli archivi cifrati usa `ARCHIVIST_PASSWORD_FILE`.
- Il backup automatico dentro l'app (Impostazioni, Backup) funziona solo ad app aperta.
- `ARCHIVIST_CONFIG_DIR` indica un'altra cartella delle impostazioni (profili separati); un agente installato con quella variabile continua a usarla.

## Come funziona (e garanzie)

- I file sono tagliati in blocchi di ~64 KB con content-defined chunking; ogni blocco è identificato dal suo hash BLAKE3 e salvato una sola volta (compresso con zstd) in pack da ~64 MB.
- Uno snapshot compare solo a scrittura completata. Un backup interrotto lascia al massimo dati orfani, ripuliti dalla prossima `prune`/`forget`. I backup precedenti non vengono mai modificati.
- L'UI ascolta solo su `127.0.0.1`, richiede un token casuale per ogni chiamata e rifiuta Host non locali (anti DNS-rebinding).
- Il ripristino rifiuta percorsi con `..` e cartelle di destinazione non vuote.

## Limiti noti (v0.1)

- **Cifratura opzionale**: `archivist init --encrypt` crea un archivio cifrato (XChaCha20-Poly1305, chiave dalla password con Argon2id); contenuti, nomi dei file e percorso del vault sono cifrati. Non sono nascosti numero e date degli snapshot e le dimensioni dei blocchi. **Se perdi la password i dati non sono recuperabili.** Puoi creare, sbloccare e cambiare la password anche dall'app (Impostazioni, Cifratura). Per cifrare un archivio in chiaro esistente usa Impostazioni → Cifratura → "Crea copia cifrata" oppure `archivist copy <nuova-cartella> --encrypt`: tutti gli snapshot vengono copiati in un nuovo archivio cifrato e l'originale resta intatto. Negli script usa `ARCHIVIST_PASSWORD` o `ARCHIVIST_PASSWORD_FILE`.
- Link simbolici ignorati; permessi Unix e attributi estesi non conservati (le date di modifica sì).
- Non usare due operazioni di scrittura sullo stesso archivio da macchine diverse contemporaneamente (c'è un lock locale ma non su rete).
- Vault con `.git` (plugin Obsidian Git): viene incluso come ogni altra cartella; aggiungi `.git` alle esclusioni se non lo vuoi.

## Lingue e tema

- Tutte le stringhe (interfaccia, motore, CLI, menu) stanno in `i18n/<codice>.json`, con segnaposto `{nome}`. Per aggiungere una lingua: copia `i18n/en.json` in `i18n/xx.json`, traducila e aggiungi una riga in `CATALOGS` di `src/i18n.rs`. Le chiavi mancanti ricadono sull'inglese.
- Lingua e tema (Sistema / Chiaro / Scuro) si scelgono da Impostazioni → Generale; il tema si cambia anche con il pulsante nella barra. Il menu nativo di macOS usa la lingua al successivo avvio.
- "Informazioni su Archivist" e "Impostazioni… ⌘," sono nel menu di sistema (macOS).
