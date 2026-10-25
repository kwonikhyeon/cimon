//! Library side of the `cimon` executable — exposed so integration tests can start the
//! service in-process.

pub mod serve;
pub mod tailscale;

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,rmcp=warn".into()))
        .with_writer(std::io::stderr)
        .try_init();
}
