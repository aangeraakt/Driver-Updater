use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::Deserializer;
use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};

const WU_DRIVER_QUERY: &str = "IsInstalled=0 and Type='Driver' and IsHidden=0";

#[derive(Debug, Clone, Deserialize)]
pub struct DriverUpdate {
    #[serde(rename = "update_id")]
    pub update_id: String,
    pub title: String,
    pub size_bytes: u64,
    #[serde(skip)]
    pub title_lower: String,
    #[serde(skip)]
    pub title_upper: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallProgress {
    pub current: u32,
    pub total: u32,
    pub title: String,
    pub result_code: i32,
}

#[derive(Debug, Clone)]
pub struct InstallSummary {
    pub results: Vec<InstallProgress>,
    pub reboot_required: bool,
}

#[derive(Debug, Deserialize)]
struct InstallSummaryJson {
    reboot_required: bool,
    #[serde(default, deserialize_with = "deserialize_install_results")]
    results: Vec<InstallProgress>,
}

fn deserialize_install_results<'de, D>(deserializer: D) -> Result<Vec<InstallProgress>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(InstallProgress),
        Many(Vec<InstallProgress>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(item)) => Ok(vec![item]),
        Some(OneOrMany::Many(items)) => Ok(items),
        None => Ok(vec![]),
    }
}

pub fn is_elevated() -> bool {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[bool](([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))",
        ])
        .output();

    match output {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            text.trim().eq_ignore_ascii_case("true")
        }
        Err(_) => false,
    }
}

pub fn request_elevation() -> Result<()> {
    let exe = std::env::current_exe().context("Kon huidig pad niet bepalen")?;
    Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Start-Process -FilePath '{}' -Verb RunAs", exe.display()),
        ])
        .spawn()
        .context("Kon niet als administrator starten")?;
    Ok(())
}

pub fn search_driver_updates() -> Result<Vec<DriverUpdate>> {
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$Session = New-Object -ComObject Microsoft.Update.Session
$Searcher = $Session.CreateUpdateSearcher()
$Searcher.Online = $true
$Result = $Searcher.Search('{WU_DRIVER_QUERY}')
$updates = @()
foreach ($Update in $Result.Updates) {{
    $updates += [PSCustomObject]@{{
        update_id = $Update.Identity.UpdateID
        title = $Update.Title
        size_bytes = [uint64]$Update.MaxDownloadSize
    }}
}}
$updates | ConvertTo-Json -Compress -Depth 3
"#
    );

    let json = run_powershell(&script)?;
    parse_driver_updates(&json)
}

pub fn install_driver_updates(update_ids: &[String]) -> Result<InstallSummary> {
    if update_ids.is_empty() {
        return Ok(InstallSummary {
            results: vec![],
            reboot_required: false,
        });
    }

    let ids_json = serde_json::to_string(update_ids).context("Kon update IDs niet serialiseren")?;
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$TargetIds = ConvertFrom-Json '{ids_json}'
$TargetSet = @{{}}
foreach ($id in $TargetIds) {{ $TargetSet[$id] = $true }}
$Session = New-Object -ComObject Microsoft.Update.Session
$Searcher = $Session.CreateUpdateSearcher()
$Searcher.Online = $true
$Result = $Searcher.Search('{WU_DRIVER_QUERY}')
$ToInstall = New-Object -ComObject Microsoft.Update.UpdateColl
foreach ($Update in $Result.Updates) {{
    if ($TargetSet.ContainsKey($Update.Identity.UpdateID)) {{
        [void]$ToInstall.Add($Update)
    }}
}}
if ($ToInstall.Count -eq 0) {{
    @{{ reboot_required = $false; results = @() }} | ConvertTo-Json -Compress -Depth 4
    exit 0
}}
$Installer = $Session.CreateUpdateInstaller()
$Installer.Updates = $ToInstall
$InstallResult = $Installer.Install()
$results = @()
for ($i = 0; $i -lt $ToInstall.Count; $i++) {{
    $results += [PSCustomObject]@{{
        current = $i + 1
        total = $ToInstall.Count
        title = $ToInstall.Item($i).Title
        result_code = $InstallResult.GetUpdateResult($i).ResultCode
    }}
}}
@{{ reboot_required = $InstallResult.RebootRequired; results = $results }} | ConvertTo-Json -Compress -Depth 4
"#
    );

    let json = run_powershell(&script)?;
    parse_install_summary(&json)
}

fn parse_driver_updates(json: &str) -> Result<Vec<DriverUpdate>> {
    let trimmed = json.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(vec![]);
    }

    let mut updates: Vec<DriverUpdate> = if trimmed.starts_with('[') {
        serde_json::from_str(trimmed).context("Kon update JSON niet parsen")?
    } else {
        vec![serde_json::from_str(trimmed).context("Kon update JSON niet parsen")?]
    };

    for update in &mut updates {
        update.title_lower = update.title.to_lowercase();
        update.title_upper = update.title.to_uppercase();
    }

    Ok(updates)
}

fn parse_install_summary(json: &str) -> Result<InstallSummary> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(InstallSummary {
            results: vec![],
            reboot_required: false,
        });
    }

    let parsed: InstallSummaryJson =
        serde_json::from_str(trimmed).context("Kon installatie resultaat niet parsen")?;
    Ok(InstallSummary {
        results: parsed.results,
        reboot_required: parsed.reboot_required,
    })
}

fn run_powershell(script: &str) -> Result<String> {
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("PowerShell start mislukt")?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(script.as_bytes())
            .context("PowerShell script schrijven mislukt")?;
    }

    let output = child
        .wait_with_output()
        .context("PowerShell wachten mislukt")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("PowerShell fout: {err}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn match_updates_to_devices(
    devices: &mut [crate::hardware::DeviceDriver],
    updates: &[DriverUpdate],
) -> HashSet<String> {
    let mut installable_ids = HashSet::new();

    for device in devices.iter_mut() {
        device.status = crate::hardware::DeviceStatus::UpToDate;
        device.update_title = None;
        device.update_id = None;

        if let Some(update) = find_best_match(device, updates) {
            device.status = crate::hardware::DeviceStatus::UpdateAvailable;
            device.update_title = Some(update.title.clone());
            device.update_id = Some(update.update_id.clone());
            installable_ids.insert(update.update_id.clone());
        }
    }

    installable_ids
}

fn find_best_match<'a>(
    device: &crate::hardware::DeviceDriver,
    updates: &'a [DriverUpdate],
) -> Option<&'a DriverUpdate> {
    let mut best: Option<(&'a DriverUpdate, usize)> = None;

    for update in updates {
        let score = score_match(device, update);
        if score > 0 && best.map_or(true, |(_, best_score)| score > best_score) {
            best = Some((update, score));
        }
    }

    best.map(|(update, _)| update)
}

fn score_match(device: &crate::hardware::DeviceDriver, update: &DriverUpdate) -> usize {
    let t = &update.title_lower;
    let mut score = 0;

    if t.contains(&device.name_lower) {
        score += 10;
    }
    if !device.manufacturer_lower.is_empty() && t.contains(&device.manufacturer_lower) {
        score += 5;
    }
    for id in &device.hardware_ids_upper {
        if update.title_upper.contains(id) {
            score += 20;
        }
    }

    score
}

pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub fn is_install_success(code: i32) -> bool {
    matches!(code, 2 | 3 | 4)
}

pub fn restart_computer() -> Result<()> {
    Command::new("shutdown")
        .args(["/r", "/t", "0"])
        .spawn()
        .context("Kon herstart niet starten")?;
    Ok(())
}

pub fn result_code_label(code: i32) -> &'static str {
    match code {
        2 => "Geslaagd",
        3 => "Geslaagd (herstart vereist)",
        4 => "Geslaagd (herstart uitgesteld)",
        5 => "Mislukt",
        _ => "Onbekend",
    }
}
