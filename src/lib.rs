use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupplyKind {
    Battery,
    Mains,
    Usb,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerSupply {
    pub name: String,
    pub kind: SupplyKind,
    pub status: Option<String>,
    pub online: Option<bool>,
    pub capacity: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerSnapshot {
    pub supplies: Vec<PowerSupply>,
    pub on_battery_only: bool,
    pub battery_capacity: Option<u8>,
}

impl PowerSnapshot {
    pub fn state_label(&self) -> &'static str {
        if self.on_battery_only {
            "battery"
        } else {
            "external-or-idle"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryState {
    pub counted_seconds: u64,
    pub on_battery_only: bool,
    pub battery_capacity: Option<u8>,
    pub last_charged_capacity: Option<u8>,
    pub discharge_seconds: u64,
    pub updated_at_unix: u64,
}

impl BatteryState {
    pub fn new(counted_seconds: u64, snapshot: &PowerSnapshot) -> Self {
        Self {
            counted_seconds,
            on_battery_only: snapshot.on_battery_only,
            battery_capacity: snapshot.battery_capacity,
            last_charged_capacity: if snapshot.on_battery_only {
                None
            } else {
                snapshot.battery_capacity
            },
            discharge_seconds: 0,
            updated_at_unix: unix_now(),
        }
    }

    pub fn next(
        previous: Option<&Self>,
        counted_seconds: u64,
        snapshot: &PowerSnapshot,
        elapsed_seconds: u64,
    ) -> Self {
        let last_charged_capacity = if snapshot.on_battery_only {
            previous
                .and_then(|state| state.last_charged_capacity)
                .or(snapshot.battery_capacity)
        } else {
            snapshot
                .battery_capacity
                .or_else(|| previous.and_then(|state| state.last_charged_capacity))
        };

        let discharge_seconds = if snapshot.on_battery_only {
            previous
                .filter(|state| state.on_battery_only)
                .map(|state| state.discharge_seconds.saturating_add(elapsed_seconds))
                .unwrap_or(elapsed_seconds)
        } else {
            0
        };

        Self {
            counted_seconds,
            on_battery_only: snapshot.on_battery_only,
            battery_capacity: snapshot.battery_capacity,
            last_charged_capacity,
            discharge_seconds,
            updated_at_unix: unix_now(),
        }
    }

    pub fn state_label(&self) -> &'static str {
        if self.on_battery_only {
            "battery"
        } else {
            "external-or-idle"
        }
    }
}

pub fn read_power_snapshot(root: impl AsRef<Path>) -> io::Result<PowerSnapshot> {
    let root = root.as_ref();
    let mut supplies = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if let Some(supply) = read_supply(&path)? {
            supplies.push(supply);
        }
    }

    if !supplies.iter().any(|s| s.kind == SupplyKind::Battery) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no battery power supply found",
        ));
    }

    let has_discharging_battery = supplies
        .iter()
        .any(|s| s.kind == SupplyKind::Battery && is_discharging(s.status.as_deref()));
    let has_online_external = supplies
        .iter()
        .any(|s| matches!(s.kind, SupplyKind::Mains | SupplyKind::Usb) && s.online == Some(true));
    let battery_capacity = supplies
        .iter()
        .filter(|s| s.kind == SupplyKind::Battery)
        .filter_map(|s| s.capacity)
        .max();

    Ok(PowerSnapshot {
        supplies,
        on_battery_only: has_discharging_battery && !has_online_external,
        battery_capacity,
    })
}

fn read_supply(path: &Path) -> io::Result<Option<PowerSupply>> {
    let Some(kind_raw) = read_trimmed(path.join("type"))? else {
        return Ok(None);
    };

    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let kind = match kind_raw.as_str() {
        "Battery" => SupplyKind::Battery,
        "Mains" => SupplyKind::Mains,
        "USB" => SupplyKind::Usb,
        other => SupplyKind::Other(other.to_string()),
    };

    Ok(Some(PowerSupply {
        name,
        kind,
        status: read_trimmed(path.join("status"))?,
        online: read_trimmed(path.join("online"))?.and_then(|v| match v.as_str() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        }),
        capacity: read_trimmed(path.join("capacity"))?.and_then(|v| v.parse().ok()),
    }))
}

fn read_trimmed(path: PathBuf) -> io::Result<Option<String>> {
    match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn is_discharging(status: Option<&str>) -> bool {
    matches!(status, Some("Discharging"))
}

pub fn read_battery_state(path: impl AsRef<Path>) -> io::Result<BatteryState> {
    let raw = fs::read_to_string(path)?;
    parse_battery_state(&raw)
}

pub fn write_battery_state(path: impl AsRef<Path>, state: &BatteryState) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let temp_path = path.with_extension("tmp");
    let mut file = fs::File::create(&temp_path)?;
    writeln!(file, "counted_seconds={}", state.counted_seconds)?;
    writeln!(file, "on_battery_only={}", state.on_battery_only)?;
    writeln!(
        file,
        "battery_capacity={}",
        state
            .battery_capacity
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )?;
    writeln!(
        file,
        "last_charged_capacity={}",
        state
            .last_charged_capacity
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    )?;
    writeln!(file, "discharge_seconds={}", state.discharge_seconds)?;
    writeln!(file, "updated_at_unix={}", state.updated_at_unix)?;
    file.sync_all()?;
    fs::rename(temp_path, path)
}

fn parse_battery_state(raw: &str) -> io::Result<BatteryState> {
    let mut counted_seconds = None;
    let mut on_battery_only = None;
    let mut battery_capacity = None;
    let mut last_charged_capacity = None;
    let mut discharge_seconds = None;
    let mut updated_at_unix = None;

    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key {
            "counted_seconds" => counted_seconds = value.parse().ok(),
            "on_battery_only" => on_battery_only = value.parse().ok(),
            "battery_capacity" => {
                battery_capacity = if value == "unknown" {
                    Some(None)
                } else {
                    Some(value.parse().ok())
                };
            }
            "last_charged_capacity" => {
                last_charged_capacity = if value == "unknown" {
                    Some(None)
                } else {
                    Some(value.parse().ok())
                };
            }
            "discharge_seconds" => discharge_seconds = value.parse().ok(),
            "updated_at_unix" => updated_at_unix = value.parse().ok(),
            _ => {}
        }
    }

    Ok(BatteryState {
        counted_seconds: counted_seconds.ok_or_else(invalid_state)?,
        on_battery_only: on_battery_only.ok_or_else(invalid_state)?,
        battery_capacity: battery_capacity.ok_or_else(invalid_state)?,
        last_charged_capacity: last_charged_capacity.unwrap_or(None),
        discharge_seconds: discharge_seconds.unwrap_or(0),
        updated_at_unix: updated_at_unix.ok_or_else(invalid_state)?,
    })
}

fn invalid_state() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid battery state file")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, remove_dir_all, write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("battery-up-test-{}-{id}", std::process::id()));
            let _ = remove_dir_all(&root);
            create_dir_all(&root).unwrap();
            Self { root }
        }

        fn supply(&self, name: &str, fields: &[(&str, &str)]) {
            let path = self.root.join(name);
            create_dir_all(&path).unwrap();
            for (field, value) in fields {
                write(path.join(field), value).unwrap();
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = remove_dir_all(&self.root);
        }
    }

    #[test]
    fn counts_when_battery_is_discharging_and_external_power_is_offline() {
        let fixture = Fixture::new();
        fixture.supply(
            "BAT1",
            &[
                ("type", "Battery\n"),
                ("status", "Discharging\n"),
                ("capacity", "42\n"),
            ],
        );
        fixture.supply("ACAD", &[("type", "Mains\n"), ("online", "0\n")]);

        let snapshot = read_power_snapshot(&fixture.root).unwrap();

        assert!(snapshot.on_battery_only);
        assert_eq!(snapshot.battery_capacity, Some(42));
    }

    #[test]
    fn does_not_count_when_usb_power_is_online() {
        let fixture = Fixture::new();
        fixture.supply(
            "BAT1",
            &[("type", "Battery\n"), ("status", "Discharging\n")],
        );
        fixture.supply(
            "ucsi-source-psy-USBC000:001",
            &[("type", "USB\n"), ("online", "1\n")],
        );

        let snapshot = read_power_snapshot(&fixture.root).unwrap();

        assert!(!snapshot.on_battery_only);
    }

    #[test]
    fn does_not_count_when_battery_is_charging() {
        let fixture = Fixture::new();
        fixture.supply(
            "BAT1",
            &[
                ("type", "Battery\n"),
                ("status", "Charging\n"),
                ("capacity", "80\n"),
            ],
        );
        fixture.supply("ACAD", &[("type", "Mains\n"), ("online", "1\n")]);

        let snapshot = read_power_snapshot(&fixture.root).unwrap();

        assert!(!snapshot.on_battery_only);
    }

    #[test]
    fn fails_when_no_battery_is_present() {
        let fixture = Fixture::new();
        fixture.supply("ACAD", &[("type", "Mains\n"), ("online", "1\n")]);

        let err = read_power_snapshot(&fixture.root).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn any_online_external_source_blocks_counting() {
        let fixture = Fixture::new();
        fixture.supply(
            "BAT1",
            &[("type", "Battery\n"), ("status", "Discharging\n")],
        );
        fixture.supply("ACAD", &[("type", "Mains\n"), ("online", "0\n")]);
        fixture.supply("USB-C", &[("type", "USB\n"), ("online", "1\n")]);

        let snapshot = read_power_snapshot(&fixture.root).unwrap();

        assert!(!snapshot.on_battery_only);
    }

    #[test]
    fn writes_and_reads_battery_state() {
        let fixture = Fixture::new();
        let path = fixture.root.join("state");
        let state = BatteryState {
            counted_seconds: 3661,
            on_battery_only: true,
            battery_capacity: Some(55),
            last_charged_capacity: Some(80),
            discharge_seconds: 900,
            updated_at_unix: 123,
        };

        write_battery_state(&path, &state).unwrap();

        assert_eq!(read_battery_state(&path).unwrap(), state);
    }

    #[test]
    fn reads_unknown_capacity_from_state() {
        let state = parse_battery_state(
            "counted_seconds=9\non_battery_only=false\nbattery_capacity=unknown\nupdated_at_unix=123\n",
        )
        .unwrap();

        assert_eq!(state.battery_capacity, None);
        assert_eq!(state.last_charged_capacity, None);
        assert_eq!(state.discharge_seconds, 0);
    }

    #[test]
    fn tracks_last_charged_capacity_and_discharge_seconds() {
        let plugged = PowerSnapshot {
            supplies: Vec::new(),
            on_battery_only: false,
            battery_capacity: Some(91),
        };
        let discharging = PowerSnapshot {
            supplies: Vec::new(),
            on_battery_only: true,
            battery_capacity: Some(88),
        };

        let state = BatteryState::next(None, 0, &plugged, 0);
        let state = BatteryState::next(Some(&state), 60, &discharging, 60);

        assert_eq!(state.last_charged_capacity, Some(91));
        assert_eq!(state.discharge_seconds, 60);

        let state = BatteryState::next(Some(&state), 120, &discharging, 60);

        assert_eq!(state.last_charged_capacity, Some(91));
        assert_eq!(state.discharge_seconds, 120);
    }
}
