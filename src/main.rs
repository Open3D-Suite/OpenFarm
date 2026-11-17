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

use std::path::PathBuf;
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
    let mut cfg = match Config::load_or_create(&data) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("settings: {e:#}");
            eprintln!("OpenFarm: {e:#}");
            std::process::exit(1);
        }
    };
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
    if changed {
        let _ = cfg.save(&data);
    }
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
    let listener = rt.block_on(async {
        let tries = if args.restarting { 40 } else { 1 };
        for i in 0..tries {
            match local::bind(cfg.local_port).await {
                Ok(l) => return Some(l),
                Err(_) if i + 1 < tries => tokio::time::sleep(Duration::from_millis(250)).await,
                Err(_) => {}
            }
        }
        None
    });
    let Some(listener) = listener else {
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
        let mut ctl_rx = ctl_rx;
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
    let url = format!("http://127.0.0.1:{}/", cfg.local_port);
    // Keep the runtime alive on its own threads while the window owns the main thread.
    let _guard = rt.enter();
    std::mem::forget(rt);
    openfarm::window::run(url, data, ctl_rx, cfg.role != Role::Unset);
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
