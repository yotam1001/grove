//! Free, self-hostable jam relay for Tide (protocol 1) and cross-service
//! Tide ↔ Grove rooms (protocol 2). No accounts, audio, logs or paid services.
//!
//! ```text
//! grove-jam-relay --port 8765                                   # this computer only
//! grove-jam-relay --endpoint https://jam.example.org --port 8765 # behind a TLS proxy
//! ```
//! The backend always binds 127.0.0.1. For internet use, put a same-machine TLS
//! proxy in front that overwrites `x-tide-jam-peer` with the client socket
//! address (Tide's `experiments/jam/nginx-shared.example.conf` works unchanged).
use tide_jam::relay::Endpoint;
use tide_jam::relay_server::{serve, Mode};

fn run() -> Result<(), &'static str> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!("grove-jam-relay [--endpoint https://jam.example.org] --port 8765");
        println!("Backend binds only 127.0.0.1; expose it through a TLS proxy.");
        return Ok(());
    }
    let mut endpoint = None;
    let mut port = None;
    if args.len() != 2 && args.len() != 4 {
        return Err("Provide --port and optionally --endpoint; see --help");
    }
    for pair in args.chunks(2) {
        match pair[0].as_str() {
            "--endpoint" if endpoint.is_none() => endpoint = Some(pair[1].clone()),
            "--port" if port.is_none() => {
                port = Some(pair[1].parse::<u16>().map_err(|_| "Invalid port")?)
            }
            _ => return Err("Unknown or duplicate relay option"),
        }
    }
    let port = port.filter(|p| *p != 0).ok_or("Invalid port")?;
    let mode = match endpoint {
        Some(endpoint) => Mode::Shared(
            Endpoint::parse(&endpoint)
                .ok()
                .filter(Endpoint::is_remote)
                .ok_or("The public endpoint must be canonical HTTPS")?,
        ),
        None => Mode::Local,
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot initialize relay")?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .map_err(|_| "Cannot bind loopback relay")?;
            println!("Jam relay listening on 127.0.0.1:{port} (protocols 1 and 2).");
            let (_stop, receiver) = tokio::sync::watch::channel(false);
            serve(listener, receiver, mode)
                .await
                .map_err(|_| "Relay stopped")
        })
}

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
