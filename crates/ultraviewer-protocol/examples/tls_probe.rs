//! Opens a TLS relay connection: cargo run --example tls_probe -- <ip> <certificate-name>
use ultraviewer_protocol::transport::{self, OpenOptions, Security};

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().collect();
    let (sink, _rx) = tokio::sync::mpsc::channel(4);
    let options = OpenOptions::new(args[1].clone(), 443, Security::ultraviewer(&args[2]));
    match transport::open(options, sink, |event| event).await {
        Ok(connection) => println!("OK {}", connection.address),
        Err(error) => println!("ERROR {error}"),
    }
}
