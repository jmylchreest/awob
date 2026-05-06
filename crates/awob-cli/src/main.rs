use std::path::PathBuf;
use std::process::ExitCode;

use awob_client::{Client, Send};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(version, about = "awob — wayland overlay bar client")]
struct Cli {
    /// Path to the daemon socket. Defaults to $XDG_RUNTIME_DIR/awob.sock.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Send a value to the daemon.
    ///
    /// `awob send volume 50`           => 50/100 = 50%
    /// `awob send volume 50 200`       => 50/200 = 25%
    Send {
        /// Free-form event name (volume, brightness, battery, mic, caps, …).
        event: String,
        value: f64,
        /// Optional max as a second positional. Mirrors wob's `<value> [<max>]`.
        max: Option<f64>,

        #[arg(long)]
        source: Option<String>,
        /// Override the auto-detected listener ID (defaults to the
        /// basename of the current executable).
        #[arg(long = "listener-id")]
        listener_id: Option<String>,
        #[arg(long)]
        style: Option<String>,
        #[arg(long)]
        accent: Option<String>,
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        icon: Option<String>,
        #[arg(long = "timeout")]
        timeout_ms: Option<u32>,
        /// Mark this send as user-interactive — hot-swap the active OSD
        /// even if a different `(source, event)` is currently displayed.
        /// Use for volume/brightness/mic-mute key presses. Without this
        /// flag, the send waits for the active OSD to finish.
        #[arg(long)]
        preempt: bool,
    },
    /// Query the daemon's tracked state.
    Query {
        #[arg(long)]
        source: Option<String>,
    },
    /// Theme management.
    #[command(subcommand)]
    Theme(ThemeCmd),
    /// Force-palette overlay control. Set or clear an overlay applied
    /// after the active theme's own palette + styles, regardless of
    /// what the theme itself imports. Lets you flip colour schemes
    /// without editing the theme file.
    #[command(subcommand)]
    ForcePalette(ForcePaletteCmd),
    /// Print client + daemon version info.
    Version,
}

#[derive(Subcommand, Debug)]
enum ThemeCmd {
    Set {
        name: String,
        /// Rewrite `awob.toml` so this theme survives daemon restart.
        /// Without this flag the change is in-memory only.
        #[arg(long)]
        persist: bool,
    },
    /// List every theme the daemon can resolve.
    List,
    Reload,
}

#[derive(Subcommand, Debug)]
enum ForcePaletteCmd {
    /// Install an overlay palette and reload the active theme.
    Set { path: PathBuf },
    /// Remove any active overlay and reload the active theme.
    Clear,
}

fn run(cli: Cli) -> Result<(), awob_client::Error> {
    let mut c = Client::connect_or_default(cli.socket.as_deref())?;
    match cli.cmd {
        Cmd::Send {
            event,
            value,
            max,
            source,
            listener_id,
            style,
            accent,
            app,
            icon,
            timeout_ms,
            preempt,
        } => {
            // Don't auto-set listener_id for the CLI — each `awob send`
            // invocation is a one-shot, not a long-running listener, and
            // many sends from different keybinds would trigger spurious
            // duplicate-listener warnings. Listener binaries set theirs
            // explicitly via `awob-client::Send::listener_id(...)`.
            let mut b = Send::new(event, value);
            if let Some(s) = listener_id {
                b = b.listener_id(s);
            }
            if let Some(m) = max {
                b = b.max(m);
            }
            if let Some(s) = source {
                b = b.source(s);
            }
            if let Some(s) = style {
                b = b.style(s);
            }
            if let Some(s) = accent {
                b = b.accent(s);
            }
            if let Some(s) = app {
                b = b.app(s);
            }
            if let Some(s) = icon {
                b = b.icon(s);
            }
            if let Some(t) = timeout_ms {
                b = b.timeout_ms(t);
            }
            if preempt {
                b = b.preempt(true);
            }
            c.send(b.build())
        }
        Cmd::Query { source } => {
            let entries = c.query(source)?;
            for e in entries {
                let lid = e.listener_id.as_deref().unwrap_or("-");
                println!(
                    "{lid}\t{}\tevent={}\tvalue={}\tmax={}\tage={:.1}s",
                    e.source, e.event, e.last_value, e.last_max, e.age_seconds
                );
            }
            Ok(())
        }
        Cmd::Theme(ThemeCmd::Set { name, persist }) => c.set_theme_with(name, persist),
        Cmd::Theme(ThemeCmd::List) => {
            for t in c.theme_list()? {
                let marker = if t.active { "*" } else { " " };
                let desc = t.description.as_deref().unwrap_or("");
                // <marker> <name padded to 14> <source padded to 8> <description>
                println!("{marker} {:<14} {:<8} {desc}", t.name, t.source);
            }
            Ok(())
        }
        Cmd::Theme(ThemeCmd::Reload) => c.reload(),
        Cmd::ForcePalette(ForcePaletteCmd::Set { path }) => {
            c.set_force_palette(Some(path.to_string_lossy().into_owned()))
        }
        Cmd::ForcePalette(ForcePaletteCmd::Clear) => c.set_force_palette(None),
        Cmd::Version => {
            let (daemon, proto) = c.version()?;
            println!("client: {}", env!("CARGO_PKG_VERSION"));
            println!("daemon: {daemon}");
            println!("protocol: {proto}");
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("awob: {e}");
            ExitCode::from(1)
        }
    }
}
