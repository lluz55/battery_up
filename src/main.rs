use battery_up::{
    read_battery_state, read_power_snapshot, write_battery_state, BatteryState, PowerSnapshot,
};
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

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

    let mut previous_state = match read_battery_state(&args.state_file) {
        Ok(state) => Some(state),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.to_string()),
    };
    let mut counted_seconds = previous_state
        .as_ref()
        .map(|state| state.counted_seconds)
        .unwrap_or(0);
    let mut last_tick = Instant::now();

    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_tick).as_secs();
        last_tick = now;

        let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
        if snapshot.on_battery_only {
            counted_seconds = counted_seconds.saturating_add(elapsed);
        }

        let state =
            BatteryState::next(previous_state.as_ref(), counted_seconds, &snapshot, elapsed);
        write_battery_state(&args.state_file, &state).map_err(|err| err.to_string())?;
        previous_state = Some(state);
        thread::sleep(args.interval);
    }

    let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
    let state = BatteryState::next(previous_state.as_ref(), counted_seconds, &snapshot, 0);
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
        "{{\"state\":\"{}\",\"on_battery_only\":{},\"battery_capacity\":{},\"last_charged_capacity\":{},\"discharge_seconds\":{},\"drain_per_minute\":{},\"counted_seconds\":{},\"counted_hms\":\"{}\",\"updated_at_unix\":{}}}",
        state.state_label(),
        state.on_battery_only,
        json_option_u8(state.battery_capacity),
        json_option_u8(state.last_charged_capacity),
        state.discharge_seconds,
        json_option_f64(drain_per_minute(state)),
        state.counted_seconds,
        format_duration(state.counted_seconds),
        state.updated_at_unix
    )
}

fn format_state_lines(state: &BatteryState) -> Vec<String> {
    let drain = drain_per_minute(state);

    let rows = [
        display_row(
            "Total on battery",
            color_bold(&format_duration(state.counted_seconds)),
            format_duration(state.counted_seconds),
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
            "Updated",
            color_muted(&state.updated_at_unix.to_string()),
            state.updated_at_unix.to_string(),
        ),
    ];

    card_lines("battery-up", &rows)
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

    card_lines("battery-up", &rows)
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

fn card_lines(title: &str, rows: &[String]) -> Vec<String> {
    const WIDTH: usize = 58;

    let title = format!(" {title} ");
    let top_fill = WIDTH.saturating_sub(title.len() + 2);
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(format!("╭{title}{}╮", "─".repeat(top_fill)));

    for row in rows {
        lines.push(format!("│ {row} │"));
    }

    lines.push(format!("╰{}╯", "─".repeat(WIDTH - 2)));
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

    card_lines("battery-up help", &rows)
}

fn display_row(label: &str, styled_value: String, plain_value: String) -> String {
    const INNER_WIDTH: usize = 54;
    const LABEL_WIDTH: usize = 18;

    let label = format!("{label:<LABEL_WIDTH$}");
    let visible_len = LABEL_WIDTH + 2 + UnicodeWidthStr::width(plain_value.as_str());
    let padding = INNER_WIDTH.saturating_sub(visible_len);

    format!(
        "{}  {}{}",
        color_muted(&label),
        styled_value,
        " ".repeat(padding)
    )
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
