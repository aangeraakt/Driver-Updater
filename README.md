# Rust Driver Updater

Een Windows desktop-app om hardware te scannen, beschikbare driver-updates te vinden via Windows Update, en die updates te installeren. Gebouwd in Rust met een egui-interface.

## Vereisten

- **Windows 10/11** (de app gebruikt WMI en de Windows Update API)
- **Rust** (edition 2021) — zie [Rust installeren](#rust-installeren)
- Internetverbinding voor het ophalen van updates
- **Administratorrechten** (aanbevolen bij installatie; sommige driver-updates vereisen dit)

## Rust installeren

Rust wordt geïnstalleerd via [rustup](https://rustup.rs/), de officiële toolchain-manager.

### Stap 1: Visual Studio Build Tools (Windows)

Dit project compileert tegen de MSVC-toolchain. Installeer de C++ build tools als je die nog niet hebt:

1. Download [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
2. Kies tijdens installatie de workload **Desktop development with C++**
3. Zorg dat **MSVC** en **Windows SDK** aangevinkt staan

### Stap 2: rustup installeren

Download en voer [rustup-init.exe](https://win.rustup.rs/x86_64) uit, of installeer vanuit PowerShell:

```powershell
winget install Rustlang.Rustup
```

Volg de prompts in de installer. De standaardoptie (`stable`, `msvc`) is correct voor dit project.

Open daarna een **nieuw** PowerShell- of terminalvenster zodat `PATH` wordt ververst.

### Stap 3: Installatie controleren

```powershell
rustc --version
cargo --version
```

Je zou iets moeten zien als `rustc 1.xx.x` en `cargo 1.xx.x`. Als beide commando's werken, kun je verder met [Installatie en starten](#installatie-en-starten).

## Installatie en starten

Clone de repository en bouw het project:

```powershell
git clone https://github.com/Aangeraakt/Driver-Updater.git
cd "Driver Updater"
cargo run --release
```

Voor snellere ontwikkeling zonder optimalisaties:

```powershell
cargo run
```

Het release-binary staat na het bouwen in `target/release/rust_driver_updater.exe`.

### Als administrator starten

Voor het installeren van driver-updates is het aan te raden de app als administrator te starten. In de app zie je een waarschuwing als je niet als admin draait. Bij installatie vraagt de app automatisch om verhoogde rechten via UAC.

Handmatig als admin starten:

```powershell
Start-Process "target\release\rust_driver_updater.exe" -Verb RunAs
```

## Gebruik

1. **Scannen** — Klik op *Scannen*. De app haalt alle actieve Plug-and-Play-apparaten op via WMI en controleert daarna automatisch Windows Update op beschikbare driver-updates.
2. **Overzicht** — Apparaten worden gegroepeerd per categorie (grafische kaart, netwerk, audio, enz.). Per apparaat zie je fabrikant, driverversie, datum en INF-bestand.
3. **Filteren** — Gebruik het zoekveld of vink *Alleen updates tonen* aan om snel apparaten met beschikbare updates te vinden.
4. **Updaten** — Klik op *Alle drivers updaten* om alle gevonden driver-updates via Windows Update te installeren. Bevestig in het dialoogvenster. Na installatie verschijnt een log met het resultaat per update.

Statuslabels per apparaat:

| Label | Betekenis |
|---|---|
| Up-to-date | Geen bijpassende update gevonden |
| Update beschikbaar | Er is een driver-update via Windows Update |
| Geïnstalleerd | Update is zojuist geïnstalleerd |
| Onbekend | Apparaat gevonden, maar geen driverinfo beschikbaar |

## Hoe het werkt

```
┌─────────────┐     ┌──────────────┐     ┌─────────────────┐     ┌──────────────┐
│  egui UI    │────▶│  Worker      │────▶│  WMI (hardware) │────▶│  Apparaten   │
│  (app.rs)   │     │  threads     │     │  hardware.rs    │     │  lijst       │
└─────────────┘     └──────────────┘     └─────────────────┘     └──────────────┘
       │                    │
       │                    ▼
       │            ┌──────────────────┐     ┌─────────────────┐
       │            │  PowerShell      │────▶│  Windows Update │
       │            │  updater.rs      │     │  (driver type)  │
       │            └──────────────────┘     └─────────────────┘
       │                    │
       ▼                    ▼
  Status & log      Updates matchen aan apparaten
```

### 1. Hardware scannen (`hardware.rs`)

Via WMI worden twee bronnen bevraagd:

- `Win32_PnPEntity` — alle actieve Plug-and-Play-apparaten
- `Win32_PnPSignedDriver` — ondertekende driverdetails (versie, datum, hardware-ID's, INF)

Apparaten zonder driverinfo worden alsnog getoond. Software-apparaten en systeemvirtuele devices (bijv. `SWD\`, `ROOT\`, `HTREE\`) worden overgeslagen.

### 2. Updates zoeken (`updater.rs`)

PowerShell roept de Windows Update COM API aan (`Microsoft.Update.Session`) met de query:

```
IsInstalled=0 and Type='Driver' and IsHidden=0
```

Dit levert alle niet-geïnstalleerde, zichtbare driver-updates op.

### 3. Matching

Updates worden gekoppeld aan apparaten op basis van apparaatnaam, fabrikant en hardware-ID's in de updatetitel. Het apparaat met de hoogste matchscore krijgt de status *Update beschikbaar*.

### 4. Installatie

Bij bevestiging installeert PowerShell alle gevonden driver-updates via `Microsoft.Update.UpdateInstaller`. Resultaten worden gelogd met codes:

| Code | Betekenis |
|---|---|
| 2 | Geslaagd |
| 3 | Geslaagd (herstart vereist) |
| 4 | Geslaagd (herstart uitgesteld) |
| 5 | Mislukt |

## Projectstructuur

```
src/
├── main.rs       — Entrypoint, vensterconfiguratie
├── app.rs        — egui-interface en app-flow
├── hardware.rs   — WMI hardware-scan
└── updater.rs    — Windows Update zoeken, matchen en installeren
```

## Afhankelijkheden

| Crate | Doel |
|---|---|
| eframe / egui | Desktop GUI |
| wmi | Windows Management Instrumentation |
| serde / serde_json | JSON-parsing van PowerShell-output |
| chrono | Driverdatums |
| anyhow | Foutafhandeling |

## Beperkingen

- Alleen **Windows** — WMI en Windows Update zijn niet beschikbaar op andere besturingssystemen.
- Updates komen uitsluitend via **Windows Update**; drivers van fabrikantwebsites worden niet opgehaald.
- Matching tussen apparaten en updates is heuristisch (naam/ID-vergelijking), niet 100% exact.
- Na sommige installaties is een **herstart** nodig.

## Licentie

Zie [LICENSE](LICENSE).
