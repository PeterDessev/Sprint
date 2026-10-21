//! `sprint sim`: the virtual printer served over TCP.

use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{Context, Result};
use sprint_sim::{SimConfig, SimServer};

use crate::cli::SimArgs;

/// The simulator configuration for the command-line options.
pub fn config(args: &SimArgs) -> SimConfig {
    let mut config = SimConfig {
        extruders: args.extruders,
        heated_bed: !args.no_heated_bed,
        heated_chamber: args.heated_chamber,
        time_scale: args.time_scale,
        instant_moves: args.instant_moves,
        boot_delay_ms: args.boot_delay_ms,
        checksum_error_rate: args.checksum_error_rate,
        drop_ok_rate: args.drop_ok_rate,
        advanced_ok: args.advanced_ok,
        ..SimConfig::default()
    };
    if let Some(seed) = args.seed {
        config.seed = seed;
    }
    if let Some(name) = args
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        config.machine_name = name.to_string();
    }
    config
}

/// The address clients should use: a wildcard listen address is reached
/// through loopback.
fn connect_address(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port())
        }
        IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port())
        }
        _ => addr,
    }
}

/// The lines printed once the server listens. The first line names the
/// bound address (scripts can take its last word).
pub fn banner(addr: SocketAddr, config: &SimConfig) -> Vec<String> {
    let target = connect_address(addr);
    vec![
        format!("Virtual printer listening on {addr}"),
        format!(
            "  {}, {} extruder{}, time scale {}x",
            config.machine_name,
            config.extruders,
            if config.extruders == 1 { "" } else { "s" },
            sprint_tui::format::trimmed(f64::from(config.time_scale), 3),
        ),
        format!("Connect with:  sprint tui --connect tcp://{target}"),
        format!("          or:  any host that connects over TCP, at {target}"),
        "Each client gets a freshly booted printer. Press Ctrl+C to stop.".to_string(),
    ]
}

/// Run `sprint sim`: serve until the process is stopped.
pub fn run(args: &SimArgs) -> Result<()> {
    let config = config(args);
    let server = SimServer::bind(args.tcp.as_str(), config.clone())
        .with_context(|| format!("cannot listen on {}", args.tcp))?;
    let addr = server.local_addr();
    log::info!("virtual printer listening on {addr}");
    let mut out = io::stdout().lock();
    for line in banner(addr, &config) {
        // Losing the banner (closed stdout) must not stop the server.
        if writeln!(out, "{line}").is_err() {
            break;
        }
    }
    let _ = out.flush();
    drop(out);
    serve(&server)
}

/// Wait while `server` serves: until the process is stopped, or until its
/// accept loop ends. A listener that failed is an error (the process
/// would otherwise sit there serving nothing); a server that was stopped
/// ends quietly.
fn serve(server: &SimServer) -> Result<()> {
    server
        .wait()
        .with_context(|| format!("the virtual printer on {} stopped", server.local_addr()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(extra: &[&str]) -> SimArgs {
        let mut argv = vec!["sprint", "sim"];
        argv.extend_from_slice(extra);
        match crate::cli::Cli::try_parse_from(argv).map(crate::cli::Cli::into_command) {
            Ok(crate::cli::Command::Sim(args)) => args,
            other => panic!("not sim: {other:?}"),
        }
    }

    #[test]
    fn options_map_to_the_config() {
        let c = config(&args(&[]));
        assert_eq!(c, SimConfig::default());
        let c = config(&args(&[
            "--extruders",
            "3",
            "--no-heated-bed",
            "--heated-chamber",
            "--time-scale",
            "25",
            "--instant-moves",
            "--boot-delay-ms",
            "5",
            "--checksum-error-rate",
            "0.1",
            "--drop-ok-rate",
            "0.2",
            "--advanced-ok",
            "--seed",
            "42",
            "--name",
            " Test Rig ",
        ]));
        assert_eq!(c.extruders, 3);
        assert!(!c.heated_bed && c.heated_chamber);
        assert_eq!(c.time_scale, 25.0);
        assert!(c.instant_moves && c.advanced_ok);
        assert_eq!(c.boot_delay_ms, 5);
        assert_eq!(c.checksum_error_rate, 0.1);
        assert_eq!(c.drop_ok_rate, 0.2);
        assert_eq!(c.seed, 42);
        assert_eq!(c.machine_name, "Test Rig");
        // A blank name keeps the default.
        let c = config(&args(&["--name", "  "]));
        assert_eq!(c.machine_name, SimConfig::default().machine_name);
    }

    #[test]
    fn serving_ends_when_the_server_stops() {
        let server = SimServer::bind("127.0.0.1:0", SimConfig::default()).expect("bind");
        assert!(!server.is_finished());
        let server = std::sync::Arc::new(server);
        let stopper = std::sync::Arc::clone(&server);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            stopper.stop();
        });
        // Stopped on purpose: not an error, and the wait returns.
        serve(&server).expect("stopped");
        assert!(server.is_finished());
        let _ = thread.join();
    }

    #[test]
    fn banner_names_the_address_first() {
        let addr: SocketAddr = "127.0.0.1:5000".parse().expect("addr");
        let lines = banner(addr, &SimConfig::default());
        assert_eq!(lines[0], "Virtual printer listening on 127.0.0.1:5000");
        assert!(lines[1].contains("Sprint Virtual Printer, 1 extruder, time scale 1x"));
        assert!(lines[2].ends_with("sprint tui --connect tcp://127.0.0.1:5000"));
        // Wildcard addresses are reached through loopback.
        let any: SocketAddr = "0.0.0.0:5001".parse().expect("addr");
        let lines = banner(any, &SimConfig::default());
        assert_eq!(lines[0], "Virtual printer listening on 0.0.0.0:5001");
        assert!(lines[2].ends_with("tcp://127.0.0.1:5001"));
        let v6: SocketAddr = "[::]:5002".parse().expect("addr");
        assert_eq!(connect_address(v6).to_string(), "[::1]:5002");
    }
}
