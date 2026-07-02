use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::de::Deserializer;
use serde::Deserialize;
use std::collections::HashMap;
use wmi::{COMLibrary, WMIConnection};

fn deserialize_string_or_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        One(String),
        Many(Vec<String>),
    }

    match Option::<StringOrVec>::deserialize(deserializer)? {
        Some(StringOrVec::One(s)) if !s.is_empty() => Ok(vec![s]),
        Some(StringOrVec::Many(v)) => Ok(v),
        _ => Ok(vec![]),
    }
}

fn deserialize_wmi_date<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum WmiDate {
        Text(String),
        DateTime(wmi::WMIDateTime),
    }

    let value = Option::<WmiDate>::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        WmiDate::Text(s) => s,
        WmiDate::DateTime(dt) => dt.0.format("%Y%m%d%H%M%S").to_string(),
    }))
}

#[derive(Debug, Clone)]
pub struct DeviceDriver {
    pub device_id: String,
    pub name: String,
    pub manufacturer: String,
    pub device_class: String,
    pub driver_version: String,
    pub driver_date: Option<DateTime<Utc>>,
    pub hardware_ids: Vec<String>,
    pub inf_name: String,
    pub status: DeviceStatus,
    pub update_title: Option<String>,
    pub update_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceStatus {
    Unknown,
    UpToDate,
    UpdateAvailable,
    Installed,
}

#[derive(Debug, Deserialize)]
struct PnPSignedDriver {
    #[serde(rename = "DeviceID")]
    device_id: Option<String>,
    #[serde(rename = "DeviceName")]
    device_name: Option<String>,
    #[serde(rename = "DriverVersion")]
    driver_version: Option<String>,
    #[serde(rename = "DriverDate", default, deserialize_with = "deserialize_wmi_date")]
    driver_date: Option<String>,
    #[serde(rename = "Manufacturer")]
    manufacturer: Option<String>,
    #[serde(rename = "DeviceClass")]
    device_class: Option<String>,
    #[serde(rename = "HardwareID", default, deserialize_with = "deserialize_string_or_vec")]
    hardware_id: Vec<String>,
    #[serde(rename = "InfName")]
    inf_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PnPEntity {
    #[serde(rename = "DeviceID")]
    device_id: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Manufacturer")]
    manufacturer: Option<String>,
    #[serde(rename = "PNPClass")]
    pnp_class: Option<String>,
}

pub fn scan_devices() -> Result<Vec<DeviceDriver>> {
    let com = COMLibrary::new().context("COM initialisatie mislukt")?;
    let wmi = WMIConnection::new(com).context("WMI verbinding mislukt")?;

    let entities: Vec<PnPEntity> = wmi
        .raw_query("SELECT DeviceID, Name, Manufacturer, PNPClass FROM Win32_PnPEntity WHERE Status = 'OK'")
        .context("Kon apparaten niet ophalen")?;

    let signed: Vec<PnPSignedDriver> = match wmi.raw_query(
        "SELECT DeviceID, DeviceName, DriverVersion, DriverDate, Manufacturer, DeviceClass, HardwareID, InfName FROM Win32_PnPSignedDriver",
    ) {
        Ok(drivers) => drivers,
        Err(e) => {
            eprintln!("Win32_PnPSignedDriver niet beschikbaar: {e}");
            Vec::new()
        }
    };

    let entity_map: HashMap<String, PnPEntity> = entities
        .into_iter()
        .filter_map(|e| e.device_id.clone().map(|id| (id, e)))
        .collect();

    let mut devices: Vec<DeviceDriver> = signed
        .into_iter()
        .filter_map(|d| build_device(d, &entity_map))
        .collect();

    for entity in entity_map.values() {
        let id = entity.device_id.as_deref().unwrap_or("");
        if devices.iter().any(|d| d.device_id == id) {
            continue;
        }
        if should_skip_entity(entity) {
            continue;
        }
        devices.push(DeviceDriver {
            device_id: id.to_string(),
            name: entity.name.clone().unwrap_or_else(|| "Onbekend apparaat".into()),
            manufacturer: entity.manufacturer.clone().unwrap_or_default(),
            device_class: entity.pnp_class.clone().unwrap_or_else(|| "Overig".into()),
            driver_version: String::new(),
            driver_date: None,
            hardware_ids: vec![],
            inf_name: String::new(),
            status: DeviceStatus::Unknown,
            update_title: None,
            update_id: None,
        });
    }

    devices.sort_by(|a, b| {
        a.device_class
            .cmp(&b.device_class)
            .then_with(|| a.name.cmp(&b.name))
    });

    if devices.is_empty() {
        anyhow::bail!("Geen hardware gevonden via WMI");
    }

    Ok(devices)
}

fn build_device(d: PnPSignedDriver, entities: &HashMap<String, PnPEntity>) -> Option<DeviceDriver> {
    let device_id = d.device_id.unwrap_or_default();
    if device_id.is_empty() || should_skip_id(&device_id) {
        return None;
    }

    let entity = entities.get(&device_id);
    let name = d
        .device_name
        .filter(|n| !n.is_empty())
        .or_else(|| entity.and_then(|e| e.name.clone()))
        .unwrap_or_else(|| "Onbekend apparaat".into());

    let manufacturer = d
        .manufacturer
        .filter(|m| !m.is_empty())
        .or_else(|| entity.and_then(|e| e.manufacturer.clone()))
        .unwrap_or_default();

    let device_class = d
        .device_class
        .filter(|c| !c.is_empty())
        .or_else(|| entity.and_then(|e| e.pnp_class.clone()))
        .unwrap_or_else(|| "Overig".into());

    Some(DeviceDriver {
        device_id,
        name,
        manufacturer,
        device_class,
        driver_version: d.driver_version.unwrap_or_default(),
        driver_date: parse_wmi_date(d.driver_date.as_deref()),
        hardware_ids: d.hardware_id,
        inf_name: d.inf_name.unwrap_or_default(),
        status: DeviceStatus::Unknown,
        update_title: None,
        update_id: None,
    })
}

fn should_skip_entity(entity: &PnPEntity) -> bool {
    entity
        .device_id
        .as_ref()
        .map(|id| should_skip_id(id))
        .unwrap_or(true)
}

fn should_skip_id(id: &str) -> bool {
    id.starts_with("SWD\\")
        || id.starts_with("SW\\")
        || id.contains("ROOT\\")
        || id.contains("HTREE\\")
        || id.contains("DISPLAY\\DEFAULT")
}

fn parse_wmi_date(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    if s.len() >= 14 {
        let fmt = &s[..14];
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(fmt, "%Y%m%d%H%M%S") {
            return Some(DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc));
        }
    }
    None
}

pub fn class_display_name(class: &str) -> &str {
    match class {
        "Display" | "DISPLAY" => "Grafische kaart",
        "Net" | "NET" => "Netwerk",
        "MEDIA" | "Media" => "Audio",
        "USB" => "USB",
        "SCSIAdapter" | "HDC" => "Opslag",
        "Bluetooth" => "Bluetooth",
        "System" => "Systeem",
        "Processor" => "Processor",
        "Monitor" => "Monitor",
        "Keyboard" => "Toetsenbord",
        "Mouse" => "Muis",
        "Biometric" => "Biometrie",
        "Camera" => "Camera",
        "PrintQueue" => "Printer",
        _ => class,
    }
}
