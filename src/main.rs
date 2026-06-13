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

    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(last_tick);
        last_tick = now;

        let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
        if snapshot.on_battery_only {
            counted += elapsed;
        }

        print_snapshot_to(&mut stdout, &snapshot, counted, args.json, false)
            .map_err(|err| err.to_string())?;
        thread::sleep(args.interval);
    }

    let snapshot = read_power_snapshot(&args.sysfs_root).map_err(|err| err.to_string())?;
    print_snapshot_to(&mut stdout, &snapshot, counted, args.json, true)
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
    println!(
        "Usage: battery-up [COMMAND] [OPTIONS]\n\n\
         Commands:\n\
           watch      Measure only while this command is running (default)\n\
           daemon     Persist accumulated battery-only time for systemd\n\
           status     Print the persisted daemon total\n\
           reset      Reset the persisted daemon total to zero\n\n\
         Options:\n\
           --interval <seconds>  Refresh interval, defaults to 1\n\
           --once                With watch, print current power state and exit\n\
           --live                With status, refresh from the daemon state file\n\
           --json                Print JSON output\n\
           --state-file <path>   State file, defaults to /var/lib/battery-up/state\n\
           --sysfs-root <path>   Override power_supply root for tests/debug\n\
           -h, --help            Show this help"
    );
}

fn print_snapshot(
    snapshot: &PowerSnapshot,
    counted: Duration,
    json: bool,
    final_line: bool,
) -> io::Result<()> {
    let mut stdout = io::stdout();
    print_snapshot_to(&mut stdout, snapshot, counted, json, final_line)
}

fn print_snapshot_to(
    writer: &mut impl Write,
    snapshot: &PowerSnapshot,
    counted: Duration,
    json: bool,
    final_line: bool,
) -> io::Result<()> {
    if json {
        writeln!(
            writer,
            "{{\"state\":\"{}\",\"on_battery_only\":{},\"battery_capacity\":{},\"counted_seconds\":{},\"counted_hms\":\"{}\",\"final\":{}}}",
            snapshot.state_label(),
            snapshot.on_battery_only,
            json_option_u8(snapshot.battery_capacity),
            counted.as_secs(),
            format_duration(counted.as_secs()),
            final_line
        )
    } else if final_line {
        writeln!(
            writer,
            "\nfinal: {} | state: {} | battery: {}",
            format_duration(counted.as_secs()),
            color_state(snapshot.state_label(), snapshot.on_battery_only),
            color_capacity(snapshot.battery_capacity)
        )
    } else {
        write!(
            writer,
            "\rtime on battery: {} | state: {} | battery: {}",
            format_duration(counted.as_secs()),
            color_state(snapshot.state_label(), snapshot.on_battery_only),
            color_capacity(snapshot.battery_capacity)
        )?;
        writer.flush()
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
    vec![
        format!("total: {}", format_duration(state.counted_seconds)),
        format!(
            "state: {}",
            color_state(state.state_label(), state.on_battery_only)
        ),
        format!("battery: {}", color_capacity(state.battery_capacity)),
        format!(
            "last charged: {}",
            color_capacity(state.last_charged_capacity)
        ),
        format!("drain/min: {}", color_drain(drain_per_minute(state))),
        format!("updated_at_unix: {}", state.updated_at_unix),
    ]
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
    let formatted = value
        .map(|value| format!("{value:.2}%/min"))
        .unwrap_or_else(|| "unknown".to_string());
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
