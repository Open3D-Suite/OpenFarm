// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenFarm.exe
//!
//!   OpenFarm                         the window (first run asks for this machine's role)
//!   OpenFarm --headless              no window (render nodes, tests)
//!   --data <dir>                     where settings, the farm and the caches live
//!                                    (default %LOCALAPPDATA%\OpenFarm)
//!   --role server|agent  --server <address>  --kind node|workstation  --name <name>
//!   --farm-name <name>  --no-render  --server-port <n>  --local-port <n>  --no-discovery
//!                                    set and save the role without the first-run page
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openfarm::agent::{AgentStatus, SharedStatus};
use openfarm::config::{Config, MachineKind, Role, default_data_dir};
use openfarm::local::{self, Control, LocalState};

struct Args {
    data: Option<PathBuf>,
    headless: bool,
    restarting: bool,
    role: Option<Role>,
    server: Option<String>,
    kind: Option<MachineKind>,
    name: Option<String>,
    farm_name: Option<String>,
    no_render: bool,
    server_port: Option<u16>,
    local_port: Option<u16>,
    no_discovery: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        data: None,
        headless: false,
        restarting: false,
        role: None,
        server: None,
        kind: None,
        name: None,
        farm_name: None,
        no_render: false,
        server_port: None,
        local_port: None,
        no_discovery: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--data" => a.data = Some(PathBuf::from(value("--data")?)),
            "--headless" => a.headless = true,
            "--restarting" => a.restarting = true,
            "--role" => {
                a.role = Some(match value("--role")?.as_str() {
                    "server" => Role::Server,
                    "agent" => Role::Agent,
                    other => return Err(format!("unknown role {other}")),
                })
            }
            "--server" => a.server = Some(value("--server")?),
            "--kind" => {
                a.kind = Some(match value("--kind")?.as_str() {
                    "node" => MachineKind::Node,
                    "workstation" => MachineKind::Workstation,
                    other => return Err(format!("unknown kind {other}")),
                })
            }
            "--name" => a.name = Some(value("--name")?),
            "--farm-name" => a.farm_name = Some(value("--farm-name")?),
            "--no-render" => a.no_render = true,
            "--server-port" => a.server_port = Some(value("--server-port")?.parse().map_err(|_| "bad port")?),
            "--local-port" => a.local_port = Some(value("--local-port")?.parse().map_err(|_| "bad port")?),
            "--no-discovery" => a.no_discovery = true,
            "--version" => {
                println!("OpenFarm {}", openfarm::config::VERSION);
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("OpenFarm: {e}");
            std::process::exit(2);
        }
    };
    let data = args.data.clone().unwrap_or_else(default_data_dir);
    let _ = std::fs::create_dir_all(data.join("logs"));
    init_logging(&data);
    let cfg = settings(&data, &args);
    // This machine's key pair: it signs everything this machine sends to the farm server.
    let identity = match cfg.identity() {
        Ok(i) => Arc::new(i),
        Err(e) => {
            tracing::error!("this machine's key: {e:#}");
            eprintln!("OpenFarm: {e:#}");
            std::process::exit(1);
        }
    };
    tracing::info!(version = openfarm::config::VERSION, role = ?cfg.role, data = %data.display(), key = %identity.fingerprint(), "OpenFarm starting");

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio");
    // One OpenFarm per data folder: a second start shows the first one's window.
    let Some(listener) = bind_local(&rt, cfg.local_port, args.restarting) else {
        let url = format!("http://127.0.0.1:{}/local/show", cfg.local_port);
        let _ = rt.block_on(reqwest::Client::new().post(url).send());
        return;
    };

    let (ctl_tx, ctl_rx) = tokio::sync::mpsc::unbounded_channel::<Control>();
    let status: SharedStatus = Arc::new(Mutex::new(AgentStatus::default()));
    let state = LocalState {
        cfg: Arc::new(Mutex::new(cfg.clone())),
        data: data.clone(),
        agent: status.clone(),
        identity: identity.clone(),
        http: reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build().expect("http"),
        control: ctl_tx,
    };
    rt.spawn(async move {
        if let Err(e) = local::serve(listener, state).await {
            tracing::error!("local app: {e:#}");
        }
    });
    if cfg.role == Role::Server {
        let (c, d) = (cfg.clone(), data.clone());
        rt.spawn(async move {
            if let Err(e) = openfarm::server::run(&c, &d).await {
                tracing::error!("farm server: {e:#}");
            }
        });
    }
    if cfg.role == Role::Agent || (cfg.role == Role::Server && cfg.render) {
        let (c, d, s) = (cfg.clone(), data.clone(), status.clone());
        rt.spawn(async move {
            // The server's own agent waits for the server to be listening.
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Err(e) = openfarm::agent::run(c, d, s, identity).await {
                tracing::error!("agent: {e:#}");
            }
        });
    }

    if args.headless {
        wait_headless(&rt, ctl_rx);
    }
    let url = format!("http://127.0.0.1:{}/", cfg.local_port);
    // Keep the runtime alive on its own threads while the window owns the main thread.
    let _guard = rt.enter();
    std::mem::forget(rt);
    openfarm::window::run(url, data, ctl_rx, cfg.role != Role::Unset);
}

/// This machine's settings with the options of the command line in them, saved again when any was
/// given. Ends the program if the settings can be neither read nor made.
fn settings(data: &Path, args: &Args) -> Config {
    let mut cfg = match Config::load_or_create(data) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("settings: {e:#}");
            eprintln!("OpenFarm: {e:#}");
            std::process::exit(1);
        }
    };
    if apply_args(&mut cfg, args) {
        let _ = cfg.save(data);
    }
    cfg
}

/// Puts the options of the command line into the settings. Says whether any was given.
fn apply_args(cfg: &mut Config, args: &Args) -> bool {
    let mut changed = false;
    macro_rules! set {
        ($field:ident, $value:expr_2021) => {
            if let Some(v) = $value {
                cfg.$field = v;
                changed = true;
            }
        };
    }
    set!(role, args.role);
    set!(server, args.server.clone());
    set!(kind, args.kind);
    set!(name, args.name.clone());
    set!(farm_name, args.farm_name.clone());
    set!(server_port, args.server_port);
    set!(local_port, args.local_port);
    if args.no_render {
        cfg.render = false;
        changed = true;
    }
    if args.no_discovery {
        cfg.discovery = false;
        changed = true;
    }
    changed
}

/// Takes the port of the local app, which only one OpenFarm per data folder can hold. `None` when
/// another one has it. After a restart it waits for the OpenFarm that is ending to let go of it.
fn bind_local(rt: &tokio::runtime::Runtime, port: u16, restarting: bool) -> Option<tokio::net::TcpListener> {
    rt.block_on(async {
        let tries = if restarting { 40 } else { 1 };
        for i in 0..tries {
            match local::bind(port).await {
                Ok(l) => return Some(l),
                Err(_) if i + 1 < tries => tokio::time::sleep(Duration::from_millis(250)).await,
                Err(_) => {}
            }
        }
        None
    })
}

/// Without a window: waits until OpenFarm is told to quit or to start again, and ends the process.
fn wait_headless(rt: &tokio::runtime::Runtime, mut ctl_rx: tokio::sync::mpsc::UnboundedReceiver<Control>) -> ! {
    let restart = rt.block_on(async {
        loop {
            match ctl_rx.recv().await {
                Some(Control::Quit) | None => return false,
                Some(Control::Restart) => return true,
                Some(Control::Show) => {}
            }
        }
    });
    if restart {
        openfarm::relaunch();
    }
    std::process::exit(0);
}

fn init_logging(data: &std::path::Path) {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(data.join("logs").join("openfarm.log"));
    let filter = tracing_subscriber::EnvFilter::try_from_env("OPENFARM_LOG").unwrap_or_else(|_| "info".into());
    match file {
        Ok(f) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(f)).try_init();
        }
        Err(_) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A command line with no options.
    fn no_args() -> Args {
        Args {
            data: None,
            headless: false,
            restarting: false,
            role: None,
            server: None,
            kind: None,
            name: None,
            farm_name: None,
            no_render: false,
            server_port: None,
            local_port: None,
            no_discovery: false,
        }
    }

    /// The settings of a machine that has just been set up, whatever this computer is called.
    fn new_settings() -> Config {
        let mut cfg = Config::new();
        cfg.name = "WORKSTATION-01".into();
        cfg.farm_name = "WORKSTATION-01 farm".into();
        cfg
    }

    /// The settings as they are written to `config.json`, to compare all of them at once.
    fn written(cfg: &Config) -> serde_json::Value {
        serde_json::to_value(cfg).unwrap()
    }

    #[test]
    fn without_options_the_settings_stay_as_they_are() {
        let mut cfg = new_settings();
        let before = written(&cfg);
        assert!(!apply_args(&mut cfg, &no_args()));
        assert_eq!(written(&cfg), before);
        // Where the data is, running without a window and starting again are not settings.
        let args = Args { data: Some(PathBuf::from(r"D:\farm")), headless: true, restarting: true, ..no_args() };
        assert!(!apply_args(&mut cfg, &args));
        assert_eq!(written(&cfg), before);
    }

    #[test]
    fn every_option_goes_into_its_setting_and_into_no_other() {
        let mut cfg = new_settings();
        let mut expected = written(&cfg);
        let args = Args {
            role: Some(Role::Agent),
            server: Some("192.168.1.10:47200".into()),
            kind: Some(MachineKind::Node),
            name: Some("NODE-07".into()),
            farm_name: Some("Studio farm".into()),
            no_render: true,
            server_port: Some(47200),
            local_port: Some(47202),
            no_discovery: true,
            ..no_args()
        };
        assert!(apply_args(&mut cfg, &args));
        for (setting, value) in [
            ("role", json!("agent")),
            ("server", json!("192.168.1.10:47200")),
            ("kind", json!("node")),
            ("name", json!("NODE-07")),
            ("farm_name", json!("Studio farm")),
            ("render", json!(false)),
            ("server_port", json!(47200)),
            ("local_port", json!(47202)),
            ("discovery", json!(false)),
        ] {
            assert_ne!(expected[setting], value, "{setting}");
            expected[setting] = value;
        }
        assert_eq!(written(&cfg), expected);
    }

    #[test]
    fn any_one_option_is_enough_for_the_settings_to_be_saved() {
        let one_at_a_time = [
            Args { role: Some(Role::Server), ..no_args() },
            Args { server: Some("192.168.1.10".into()), ..no_args() },
            Args { kind: Some(MachineKind::Node), ..no_args() },
            Args { name: Some("NODE-07".into()), ..no_args() },
            Args { farm_name: Some("Studio farm".into()), ..no_args() },
            Args { no_render: true, ..no_args() },
            Args { server_port: Some(47200), ..no_args() },
            Args { local_port: Some(47202), ..no_args() },
            Args { no_discovery: true, ..no_args() },
        ];
        for (i, args) in one_at_a_time.iter().enumerate() {
            let mut cfg = new_settings();
            let before = written(&cfg);
            assert!(apply_args(&mut cfg, args), "option {i}");
            let changed = written(&cfg);
            let differ = before.as_object().unwrap().iter().filter(|(setting, value)| changed[setting.as_str()] != **value).count();
            assert_eq!(differ, 1, "option {i}");
        }
    }

    #[test]
    fn an_option_that_repeats_the_settings_still_counts_as_given() {
        let mut cfg = new_settings();
        cfg.role = Role::Server;
        cfg.render = false;
        let before = written(&cfg);
        let args = Args { role: Some(Role::Server), kind: Some(cfg.kind), server_port: Some(cfg.server_port), no_render: true, ..no_args() };
        assert!(apply_args(&mut cfg, &args));
        assert_eq!(written(&cfg), before);
    }
}
