mod bridge;
mod message;
mod proxy;
mod transport;

use clap::Parser;
use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser, Debug)]
#[command(name = "helix-vue-proxy")]
#[command(about = "LSP proxy bridging Helix and vue-language-server via tsserver protocol")]
struct Args {
    /// Path to vue-language-server binary
    #[arg(long, default_value = "vue-language-server")]
    vue_server_path: String,

    /// Path to typescript-language-server binary
    #[arg(long, default_value = "typescript-language-server")]
    ts_server_path: String,

    /// Path to @vue/typescript-plugin
    #[arg(long)]
    plugin_path: String,

    /// Path to TypeScript SDK (tsdk)
    #[arg(long)]
    tsdk: String,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "warn")]
    log_level: String,

    /// Optional log file path (in addition to stderr)
    #[arg(long)]
    log_file: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let level: Level = args.log_level.parse().unwrap_or(Level::WARN);
    let filter = EnvFilter::from_default_env()
        .add_directive(format!("helix_vue_proxy={level}").parse().unwrap());

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr);

    if let Some(ref log_path) = args.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .map_err(|e| anyhow::anyhow!("failed to open log file {log_path}: {e}"))?;

        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false);

        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .init();
    }

    tracing::info!("starting helix-vue-proxy");
    tracing::info!(vue_server = %args.vue_server_path, ts_server = %args.ts_server_path);
    tracing::info!(plugin_path = %args.plugin_path, tsdk = %args.tsdk);

    proxy::run(
        &args.vue_server_path,
        &args.ts_server_path,
        &args.plugin_path,
        &args.tsdk,
    )
    .await
}
