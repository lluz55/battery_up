use battery_up_core::{
    read_battery_state, read_power_snapshot, write_battery_state, BatteryHistoryPoint,
    BatteryState, PowerSnapshot,
};
use std::env;
use std::ffi::CStr;
use std::io::{self, Write};
use std::os::raw::{c_char, c_int, c_long};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const SYSFS_POWER_SUPPLY: &str = "/sys/class/power_supply";
const DEFAULT_STATE_FILE: &str = "/var/lib/battery-up/state";
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_: i32) {
    STOP.store(true, Ordering::SeqCst);
}

extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> extern "C" fn(i32);
    fn localtime_r(timep: *const c_long, result: *mut Tm) -> *mut Tm;
    fn strftime(s: *mut c_char, max: usize, format: *const c_char, tm: *const Tm) -> usize;
}

#[repr(C)]
struct Tm {
    tm_sec: c_int,
    tm_min: c_int,
    tm_hour: c_int,
    tm_mday: c_int,
    tm_mon: c_int,
    tm_year: c_int,
    tm_wday: c_int,
    tm_yday: c_int,
    tm_isdst: c_int,
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

#[derive(Debug)]
enum Command {
    Watch,
    Daemon,
    Status,
    Reset,
}

#[derive(Debug)]
struct Args {
    command: Command,
    interval: Duration,
    once: bool,
    live: bool,
    json: bool,
    sysfs_root: String,
    state_file: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("battery-up: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args(env::args().skip(1))?;

    match args.command {
        Command::Watch => run_watch(&args),
        Command::Daemon => run_daemon(&args),
        Command::Status => run_status(&args),
        Command::Reset => run_reset(&args),
    }
}

fn run_watch(args: &Args) -> Result<(), String> {
    if args.once {
        let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
        print_snapshot(&snapshot, Duration::ZERO, args.json, true)
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    install_signal_handlers();

    let mut counted = Duration::ZERO;
    let mut last_tick = Instant::now();
    let mut stdout = io::stdout();
    let mut rendered = false;

    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_tick);
        last_tick = now;

        let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
        if snapshot.on_battery_only {
            counted += elapsed;
        }

        print_snapshot_to(&mut stdout, &snapshot, counted, args.json, false, rendered)
            .map_err(|err| err.to_string())?;
        rendered = true;
        thread::sleep(args.interval);
    }

    let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
    print_snapshot_to(&mut stdout, &snapshot, counted, args.json, true, rendered)
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn run_daemon(args: &Args) -> Result<(), String> {
    install_signal_handlers();

    let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
    let initial_state = BatteryState::new(0, &snapshot);
    write_battery_state(&args.state_file, &initial_state).map_err(|err| err.to_string())?;

    let mut previous_state = Some(initial_state);
    let mut counted_seconds: u64 = 0;
    let mut last_tick = Instant::now();
    let mut can_detect_standby = false;

    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_tick).as_secs();
        last_tick = now;

        let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
        let wall_elapsed = previous_state
            .as_ref()
            .map(|state| current_unix().saturating_sub(state.updated_at_unix))
            .unwrap_or(elapsed);
        let standby_elapsed = if can_detect_standby {
            wall_elapsed.saturating_sub(elapsed)
        } else {
            0
        };
        if snapshot.on_battery_only {
            counted_seconds = counted_seconds.saturating_add(elapsed);
        }

        let state = BatteryState::next(
            previous_state.as_ref(),
            counted_seconds,
            &snapshot,
            elapsed,
            standby_elapsed,
        );
        write_battery_state(&args.state_file, &state).map_err(|err| err.to_string())?;
        counted_seconds = state.counted_seconds;
        previous_state = Some(state);
        can_detect_standby = true;
        thread::sleep(args.interval);
    }

    let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
    let state = BatteryState::next(previous_state.as_ref(), counted_seconds, &snapshot, 0, 0);
    write_battery_state(&args.state_file, &state).map_err(|err| err.to_string())
}

fn run_status(args: &Args) -> Result<(), String> {
    if args.live {
        return run_live_status(args);
    }

    let state = read_battery_state(&args.state_file).map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => format!(
            "state file not found at {}; start the systemd service or run `battery-up daemon`",
            args.state_file.display()
        ),
        _ => err.to_string(),
    })?;
    print_state(&state, args.json).map_err(|err| err.to_string())
}

fn run_live_status(args: &Args) -> Result<(), String> {
    install_signal_handlers();

    let mut stdout = io::stdout();
    let mut rendered = false;
    while !STOP.load(Ordering::SeqCst) {
        let state = read_battery_state(&args.state_file).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => format!(
                "state file not found at {}; start the systemd service or run `battery-up daemon`",
                args.state_file.display()
            ),
            _ => err.to_string(),
        })?;
        print_live_state_to(&mut stdout, &state, args.json, rendered)
            .map_err(|err| err.to_string())?;
        rendered = true;
        thread::sleep(args.interval);
    }

    Ok(())
}

fn run_reset(args: &Args) -> Result<(), String> {
    let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
    let state = BatteryState::new(0, &snapshot);
    write_battery_state(&args.state_file, &state).map_err(|err| err.to_string())?;
    print_state(&state, args.json).map_err(|err| err.to_string())
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut command = Command::Watch;
    let mut interval = Duration::from_secs(1);
    let mut once = false;
    let mut live = false;
    let mut json = false;
    let mut sysfs_root = SYSFS_POWER_SUPPLY.to_string();
    let mut state_file = PathBuf::from(DEFAULT_STATE_FILE);

    if let Some(first) = args.next() {
        match first.as_str() {
            "watch" => command = Command::Watch,
            "daemon" => command = Command::Daemon,
            "status" => command = Command::Status,
            "reset" => command = Command::Reset,
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                print_version();
                std::process::exit(0);
            }
            other => parse_option(
                other.to_string(),
                &mut args,
                &mut interval,
                &mut once,
                &mut live,
                &mut json,
                &mut sysfs_root,
                &mut state_file,
            )?,
        }
    }

    while let Some(arg) = args.next() {
        parse_option(
            arg,
            &mut args,
            &mut interval,
            &mut once,
            &mut live,
            &mut json,
            &mut sysfs_root,
            &mut state_file,
        )?;
    }

    Ok(Args {
        command,
        interval,
        once,
        live,
        json,
        sysfs_root,
        state_file,
    })
}

fn parse_option(
    arg: String,
    args: &mut impl Iterator<Item = String>,
    interval: &mut Duration,
    once: &mut bool,
    live: &mut bool,
    json: &mut bool,
    sysfs_root: &mut String,
    state_file: &mut PathBuf,
) -> Result<(), String> {
    match arg.as_str() {
        "--interval" => {
            let value = args
                .next()
                .ok_or_else(|| "--interval requires a value in seconds".to_string())?;
            let seconds: u64 = value
                .parse()
                .map_err(|_| "--interval must be a positive integer".to_string())?;
            if seconds == 0 {
                return Err("--interval must be at least 1".to_string());
            }
            *interval = Duration::from_secs(seconds);
        }
        "--once" => *once = true,
        "--live" => *live = true,
        "--json" => *json = true,
        "--sysfs-root" => {
            *sysfs_root = args
                .next()
                .ok_or_else(|| "--sysfs-root requires a path".to_string())?;
        }
        "--state-file" => {
            *state_file = PathBuf::from(
                args.next()
                    .ok_or_else(|| "--state-file requires a path".to_string())?,
            );
        }
        "-h" | "--help" => {
            print_help();
            std::process::exit(0);
        }
        "-V" | "--version" => {
            print_version();
            std::process::exit(0);
        }
        other => return Err(format!("unknown argument: {other}")),
    }

    Ok(())
}

fn install_signal_handlers() {
    unsafe {
        signal(SIGINT, handle_signal);
        signal(SIGTERM, handle_signal);
    }
}

fn print_help() {
    for line in help_lines() {
        println!("{line}");
    }
}

fn print_version() {
    println!("battery-up {}", app_version());
}

fn app_version() -> &'static str {
    option_env!("BATTERY_UP_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn print_snapshot(
    snapshot: &PowerSnapshot,
    counted: Duration,
    json: bool,
    final_line: bool,
) -> io::Result<()> {
    let mut stdout = io::stdout();
    print_snapshot_to(&mut stdout, snapshot, counted, json, final_line, false)
}

fn print_snapshot_to(
    writer: &mut impl Write,
    snapshot: &PowerSnapshot,
    counted: Duration,
    json: bool,
    final_line: bool,
    redraw_previous: bool,
) -> io::Result<()> {
    if json {
        let line = format!(
            "{{\"state\":\"{}\",\"on_battery_only\":{},\"battery_capacity\":{},\"counted_seconds\":{},\"counted_hms\":\"{}\",\"final\":{}}}",
            snapshot.state_label(),
            snapshot.on_battery_only,
            json_option_u8(snapshot.battery_capacity),
            counted.as_secs(),
            format_duration(counted.as_secs()),
            final_line
        );
        if final_line && redraw_previous {
            write!(writer, "\x1b[1G\x1b[2K")?;
        }
        write_status_line(writer, &line, final_line)
    } else {
        write_status_block(
            writer,
            &format_snapshot_lines(snapshot, counted),
            redraw_previous,
            final_line,
        )
    }
}

fn print_state(state: &BatteryState, json: bool) -> io::Result<()> {
    let mut stdout = io::stdout();
    print_state_to(&mut stdout, state, json, true)
}

fn print_state_to(
    writer: &mut impl Write,
    state: &BatteryState,
    json: bool,
    final_line: bool,
) -> io::Result<()> {
    if json {
        write_status_line(writer, &format_state_json(state), final_line)
    } else {
        write_status_block(writer, &format_state_lines(state), false, final_line)
    }
}

fn print_live_state_to(
    writer: &mut impl Write,
    state: &BatteryState,
    json: bool,
    redraw_previous: bool,
) -> io::Result<()> {
    if json {
        write_status_line(writer, &format_state_json(state), false)
    } else {
        write_status_block(writer, &format_state_lines(state), redraw_previous, false)
    }
}

fn write_status_line(writer: &mut impl Write, line: &str, final_line: bool) -> io::Result<()> {
    if final_line {
        writeln!(writer, "{line}")
    } else {
        write!(writer, "\x1b[1G\x1b[2K{line}")?;
        writer.flush()
    }
}

fn write_status_block(
    writer: &mut impl Write,
    lines: &[String],
    redraw_previous: bool,
    final_block: bool,
) -> io::Result<()> {
    if redraw_previous && lines.len() > 1 {
        write!(writer, "\x1b[{}A", lines.len() - 1)?;
    }

    for (index, line) in lines.iter().enumerate() {
        if final_block {
            write!(writer, "{line}")?;
        } else {
            write!(writer, "\x1b[1G\x1b[2K{line}")?;
        }
        if index + 1 < lines.len() || final_block {
            writeln!(writer)?;
        }
    }

    writer.flush()
}

fn format_state_json(state: &BatteryState) -> String {
    format!(
        "{{\"state\":\"{}\",\"on_battery_only\":{},\"battery_capacity\":{},\"last_charged_capacity\":{},\"discharge_seconds\":{},\"drain_per_minute\":{},\"standby_seconds\":{},\"standby_hms\":\"{}\",\"active_drop_percent\":{},\"standby_drop_percent\":{},\"standby_drain_per_minute\":{},\"counted_seconds\":{},\"counted_hms\":\"{}\",\"total_battery_seconds\":{},\"total_battery_hms\":\"{}\",\"updated_at_unix\":{},\"history\":[{}]}}",
        state.state_label(),
        state.on_battery_only,
        json_option_u8(state.battery_capacity),
        json_option_u8(state.last_charged_capacity),
        state.discharge_seconds,
        json_option_f64(drain_per_minute(state)),
        state.standby_seconds,
        format_duration(state.standby_seconds),
        state.active_drop_percent,
        state.standby_drop_percent,
        json_option_f64(standby_drain_per_minute(state)),
        state.counted_seconds,
        format_duration(state.counted_seconds),
        total_battery_seconds(state),
        format_duration(total_battery_seconds(state)),
        state.updated_at_unix,
        format_history_json(&state.history)
    )
}

fn format_state_lines(state: &BatteryState) -> Vec<String> {
    let drain = drain_per_minute(state);
    let standby_drain = standby_drain_per_minute(state);
    let updated_at = format_human_time(state.updated_at_unix);

    let rows = [
        display_row(
            "Battery active",
            color_bold(&format_duration(state.counted_seconds)),
            format_duration(state.counted_seconds),
        ),
        display_row(
            "Battery standby",
            color_bold(&format_duration(state.standby_seconds)),
            format_duration(state.standby_seconds),
        ),
        display_row(
            "Battery total",
            color_bold(&format_duration(total_battery_seconds(state))),
            format_duration(total_battery_seconds(state)),
        ),
        display_row(
            "Power state",
            state_badge(state.state_label(), state.on_battery_only),
            format!("● {}", state.state_label()),
        ),
        display_row(
            "Battery",
            capacity_meter(state.battery_capacity),
            plain_capacity_meter(state.battery_capacity),
        ),
        display_row(
            "Last charged",
            color_capacity(state.last_charged_capacity),
            format_capacity(state.last_charged_capacity),
        ),
        display_row("Drain rate", color_drain(drain), format_drain(drain)),
        display_row(
            "Active drop",
            color_bold(&format_drop(state.active_drop_percent)),
            format_drop(state.active_drop_percent),
        ),
        display_row(
            "Standby drop",
            color_bold(&format_drop(state.standby_drop_percent)),
            format_drop(state.standby_drop_percent),
        ),
        display_row(
            "Standby drain",
            color_drain(standby_drain),
            format_drain(standby_drain),
        ),
        display_row("Updated", color_muted(&updated_at), updated_at),
    ];

    let mut lines = section_lines("battery-up", &rows);
    lines.extend(format_drop_chart(&state.history));
    lines
}

fn format_snapshot_lines(snapshot: &PowerSnapshot, counted: Duration) -> Vec<String> {
    let rows = [
        display_row(
            "Session on battery",
            color_bold(&format_duration(counted.as_secs())),
            format_duration(counted.as_secs()),
        ),
        display_row(
            "Power state",
            state_badge(snapshot.state_label(), snapshot.on_battery_only),
            format!("● {}", snapshot.state_label()),
        ),
        display_row(
            "Battery",
            capacity_meter(snapshot.battery_capacity),
            plain_capacity_meter(snapshot.battery_capacity),
        ),
    ];

    section_lines("battery-up", &rows)
}

fn format_duration(total: u64) -> String {
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_capacity(capacity: Option<u8>) -> String {
    capacity
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_drop(drop_percent: u64) -> String {
    match drop_percent {
        0 => "no battery used".to_string(),
        1 => "1% battery used".to_string(),
        value => format!("{value}% battery used"),
    }
}

fn capacity_meter(capacity: Option<u8>) -> String {
    match capacity {
        Some(value) => format!(
            "{} {}",
            color_capacity_bar(value, true),
            color_capacity(Some(value))
        ),
        None => color_capacity(None),
    }
}

fn plain_capacity_meter(capacity: Option<u8>) -> String {
    match capacity {
        Some(value) => format!(
            "{} {}",
            capacity_bar(value, false),
            format_capacity(Some(value))
        ),
        None => format_capacity(None),
    }
}

fn color_capacity_bar(capacity: u8, compact: bool) -> String {
    let bar = capacity_bar(capacity, compact);

    match capacity {
        value if value >= 70 => color(&bar, "32"),
        value if value >= 30 => color(&bar, "33"),
        _ => color(&bar, "31"),
    }
}

fn capacity_bar(capacity: u8, compact: bool) -> String {
    let width = if compact { 10usize } else { 18usize };
    let filled = (usize::from(capacity).min(100) * width + 50) / 100;
    let empty = width.saturating_sub(filled);
    format!("{}{}", "▰".repeat(filled), "▱".repeat(empty))
}

fn drain_per_minute(state: &BatteryState) -> Option<f64> {
    if !state.on_battery_only || state.discharge_seconds == 0 {
        return None;
    }

    let start = state.last_charged_capacity?;
    let current = state.battery_capacity?;
    if start <= current {
        return None;
    }

    Some((f64::from(start - current) * 60.0) / state.discharge_seconds as f64)
}

fn standby_drain_per_minute(state: &BatteryState) -> Option<f64> {
    if state.standby_seconds == 0 || state.standby_drop_percent == 0 {
        return None;
    }

    Some((state.standby_drop_percent as f64 * 60.0) / state.standby_seconds as f64)
}

fn total_battery_seconds(state: &BatteryState) -> u64 {
    state.counted_seconds.saturating_add(state.standby_seconds)
}

fn format_drop_chart(history: &[BatteryHistoryPoint]) -> Vec<String> {
    const CHART_WIDTH: usize = 18;

    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push(color_bold("Drain history"));

    if history.len() < 2 {
        lines.push(color_muted("needs at least 2 samples"));
        return lines;
    }

    let start = history.len().saturating_sub(CHART_WIDTH);
    let points = &history[start..];
    let max_drop = points
        .iter()
        .flat_map(|point| [point.active_drop_percent, point.standby_drop_percent])
        .max()
        .unwrap_or(0)
        .max(1);

    let active_line = sparkline(
        points.iter().map(|point| point.active_drop_percent),
        max_drop,
    );
    let standby_line = sparkline(
        points.iter().map(|point| point.standby_drop_percent),
        max_drop,
    );

    lines.push(display_row(
        "Active",
        format!(
            "{} {}",
            color(&active_line, "33"),
            color_bold(&format_drop(points.last().unwrap().active_drop_percent))
        ),
        format!(
            "{} {}",
            active_line,
            format_drop(points.last().unwrap().active_drop_percent)
        ),
    ));
    lines.push(display_row(
        "Standby",
        format!(
            "{} {}",
            color(&standby_line, "36"),
            color_bold(&format_drop(points.last().unwrap().standby_drop_percent))
        ),
        format!(
            "{} {}",
            standby_line,
            format_drop(points.last().unwrap().standby_drop_percent)
        ),
    ));
    lines.push(display_row(
        "Scale",
        color_muted(&format!("0% to {max_drop}% battery used")),
        format!("0% to {max_drop}% battery used"),
    ));
    lines
}

fn sparkline(values: impl Iterator<Item = u64>, max_value: u64) -> String {
    const TICKS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

    values
        .map(|value| {
            let index = if max_value == 0 {
                0
            } else {
                (value.saturating_mul((TICKS.len() - 1) as u64) + max_value / 2) / max_value
            };
            TICKS[index as usize]
        })
        .collect()
}

fn current_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_human_time(unix: u64) -> String {
    let timestamp = unix.min(c_long::MAX as u64) as c_long;
    let mut tm = Tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    };
    let mut buffer = [0 as c_char; 64];
    let format = c"%Y-%m-%d %H:%M:%S %Z";

    unsafe {
        if localtime_r(&timestamp, &mut tm).is_null() {
            return unix.to_string();
        }
        if strftime(buffer.as_mut_ptr(), buffer.len(), format.as_ptr(), &tm) == 0 {
            return unix.to_string();
        }
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

fn color_state(label: &str, on_battery_only: bool) -> String {
    if on_battery_only {
        color(label, "33")
    } else {
        color(label, "32")
    }
}

fn color_capacity(capacity: Option<u8>) -> String {
    let formatted = format_capacity(capacity);
    match capacity {
        Some(value) if value >= 70 => color(&formatted, "32"),
        Some(value) if value >= 30 => color(&formatted, "33"),
        Some(_) => color(&formatted, "31"),
        None => color(&formatted, "90"),
    }
}

fn color_drain(value: Option<f64>) -> String {
    let formatted = format_drain(value);
    match value {
        Some(value) if value <= 0.20 => color(&formatted, "32"),
        Some(value) if value <= 0.60 => color(&formatted, "33"),
        Some(_) => color(&formatted, "31"),
        None => color(&formatted, "90"),
    }
}

fn color(value: &str, code: &str) -> String {
    format!("\x1b[{code}m{value}\x1b[0m")
}

fn section_lines(title: &str, rows: &[String]) -> Vec<String> {
    let mut lines = Vec::with_capacity(rows.len() + 1);
    lines.push(color_bold(title));
    lines.extend(rows.iter().cloned());
    lines
}

fn help_lines() -> Vec<String> {
    let rows = [
        display_row(
            "Usage",
            color_bold("battery-up [command] [options]"),
            "battery-up [command] [options]".to_string(),
        ),
        display_row(
            "watch",
            "measure this terminal session".to_string(),
            "measure this terminal session".to_string(),
        ),
        display_row(
            "daemon",
            "persist time for systemd".to_string(),
            "persist time for systemd".to_string(),
        ),
        display_row(
            "status",
            "show persisted total".to_string(),
            "show persisted total".to_string(),
        ),
        display_row(
            "reset",
            "reset persisted total".to_string(),
            "reset persisted total".to_string(),
        ),
        display_row(
            "--interval <sec>",
            "refresh interval".to_string(),
            "refresh interval".to_string(),
        ),
        display_row(
            "--once",
            "print once and exit".to_string(),
            "print once and exit".to_string(),
        ),
        display_row(
            "--live",
            "refresh status in place".to_string(),
            "refresh status in place".to_string(),
        ),
        display_row(
            "--json",
            "machine-readable output".to_string(),
            "machine-readable output".to_string(),
        ),
        display_row(
            "-V, --version",
            "show version and exit".to_string(),
            "show version and exit".to_string(),
        ),
        display_row(
            "--state-file",
            "/var/lib/battery-up/state".to_string(),
            "/var/lib/battery-up/state".to_string(),
        ),
        display_row(
            "--sysfs-root",
            "/sys/class/power_supply".to_string(),
            "/sys/class/power_supply".to_string(),
        ),
    ];

    section_lines("battery-up help", &rows)
}

fn display_row(label: &str, styled_value: String, _plain_value: String) -> String {
    const LABEL_WIDTH: usize = 18;

    let label = format!("{label:<LABEL_WIDTH$}");

    format!("{}  {}", color_muted(&label), styled_value)
}

fn state_badge(label: &str, on_battery_only: bool) -> String {
    let badge = format!("● {label}");
    color_state(&badge, on_battery_only)
}

fn format_drain(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.2}%/min"))
        .unwrap_or_else(|| "unknown".to_string())
}

fn color_bold(value: &str) -> String {
    color(value, "1")
}

fn color_muted(value: &str) -> String {
    color(value, "90")
}

fn json_option_u8(value: Option<u8>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn json_option_f64(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "null".to_string())
}

fn format_history_json(history: &[BatteryHistoryPoint]) -> String {
    history
        .iter()
        .map(|point| {
            format!(
                "{{\"updated_at_unix\":{},\"active_drop_percent\":{},\"standby_drop_percent\":{},\"battery_capacity\":{}}}",
                point.updated_at_unix,
                point.active_drop_percent,
                point.standby_drop_percent,
                json_option_u8(point.battery_capacity)
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_history(history: Vec<BatteryHistoryPoint>) -> BatteryState {
        BatteryState {
            counted_seconds: 120,
            standby_seconds: 60,
            on_battery_only: true,
            battery_capacity: Some(87),
            last_charged_capacity: Some(91),
            discharge_seconds: 120,
            active_drop_percent: history
                .last()
                .map(|point| point.active_drop_percent)
                .unwrap_or(0),
            standby_drop_percent: history
                .last()
                .map(|point| point.standby_drop_percent)
                .unwrap_or(0),
            history,
            updated_at_unix: 123,
        }
    }

    #[test]
    fn status_renders_chart_with_enough_history() {
        let state = state_with_history(vec![
            BatteryHistoryPoint {
                updated_at_unix: 1,
                active_drop_percent: 0,
                standby_drop_percent: 0,
                battery_capacity: Some(91),
            },
            BatteryHistoryPoint {
                updated_at_unix: 2,
                active_drop_percent: 3,
                standby_drop_percent: 1,
                battery_capacity: Some(87),
            },
        ]);

        let rendered = format_state_lines(&state).join("\n");

        assert!(rendered.contains("Drain history"));
        assert!(rendered.contains("Active"));
        assert!(rendered.contains("Standby"));
        assert!(rendered.contains("Scale"));
        assert!(rendered.contains("3% battery used"));
        assert!(rendered.contains("1% battery used"));
        assert!(rendered.contains("█"));
    }

    #[test]
    fn status_renders_chart_fallback_without_enough_history() {
        let state = state_with_history(vec![BatteryHistoryPoint {
            updated_at_unix: 1,
            active_drop_percent: 0,
            standby_drop_percent: 0,
            battery_capacity: Some(91),
        }]);

        let rendered = format_state_lines(&state).join("\n");

        assert!(rendered.contains("Drain history"));
        assert!(rendered.contains("needs at least 2 samples"));
    }

    #[test]
    fn status_json_includes_drop_history() {
        let state = state_with_history(vec![
            BatteryHistoryPoint {
                updated_at_unix: 1,
                active_drop_percent: 0,
                standby_drop_percent: 0,
                battery_capacity: Some(91),
            },
            BatteryHistoryPoint {
                updated_at_unix: 2,
                active_drop_percent: 3,
                standby_drop_percent: 1,
                battery_capacity: None,
            },
        ]);

        let json = format_state_json(&state);

        assert!(json.contains("\"active_drop_percent\":3"));
        assert!(json.contains("\"history\":["));
        assert!(json.contains("\"updated_at_unix\":2"));
        assert!(json.contains("\"battery_capacity\":null"));
        assert!(json.contains("\"counted_seconds\":120"));
    }
}
