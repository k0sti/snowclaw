use clap::{Parser, Subcommand};
use anyhow::{Result, Context};
use tokio::signal;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod cache;
mod profiles;
mod relay;
mod webhook;
mod api;
mod bridge;

use config::Config;
use relay::create_keys_from_nsec;
use bridge::Bridge;
use api::ApiServer;

#[derive(Parser)]
#[command(name = "bridge")]
#[command(about = "Rust Nostr bridge for OpenClaw")]
#[command(version = "1.0.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Configuration file path
    #[arg(short, long, default_value = "bridge.toml")]
    config: String,

    /// Test configuration and exit
    #[arg(long)]
    test: bool,

    /// Show version and exit
    #[arg(long)]
    version: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the bridge
    Run,
    /// Test configuration
    Test,
    /// Show version
    Version,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.version {
        println!("bridge v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Load configuration
    let mut config = Config::load_from_file(&cli.config)
        .with_context(|| format!("Failed to load config from {}", cli.config))?;

    config.expand_paths()
        .with_context(|| "Failed to expand paths in config")?;

    // Initialize logging
    init_logging(&config.logging.level)?;

    // Validate configuration
    config.validate()
        .with_context(|| "Configuration validation failed")?;

    if cli.test {
        return test_config(&config).await;
    }

    // Default to run command if no subcommand specified
    let command = cli.command.unwrap_or(Commands::Run);

    match command {
        Commands::Run => run_bridge(config).await,
        Commands::Test => test_config(&config).await,
        Commands::Version => {
            println!("bridge v{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

async fn run_bridge(config: Config) -> Result<()> {
    tracing::info!("Starting Nostr bridge v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configuration loaded from bridge.toml");

    // Load identity
    let identity = config.load_identity()
        .with_context(|| "Failed to load identity")?;

    // Create nostr keys
    let keys = create_keys_from_nsec(&identity.nsec)
        .with_context(|| "Failed to create keys from nsec")?;

    tracing::info!("Loaded identity: {}", keys.public_key());

    // Create and start bridge
    let mut bridge = Bridge::new(config.clone(), keys).await
        .with_context(|| "Failed to create bridge")?;

    bridge.start().await
        .with_context(|| "Failed to start bridge")?;

    // Start API server
    let api_server = ApiServer::new(config.api.bind.clone());
    let bridge_state = bridge.state();
    
    tokio::spawn(async move {
        if let Err(e) = api_server.start(bridge_state).await {
            tracing::error!("API server error: {}", e);
        }
    });

    // Wait for shutdown signal
    wait_for_shutdown().await?;

    // Graceful shutdown
    tracing::info!("Received shutdown signal, stopping bridge...");
    bridge.shutdown().await
        .with_context(|| "Failed to shutdown bridge")?;

    Ok(())
}

async fn test_config(config: &Config) -> Result<()> {
    println!("Testing configuration...");

    // Test configuration validity
    config.validate()
        .with_context(|| "Configuration validation failed")?;
    println!("✓ Configuration is valid");

    // Test identity loading
    let identity = config.load_identity()
        .with_context(|| "Failed to load identity")?;
    println!("✓ Identity file is readable");

    // Test key parsing
    let keys = create_keys_from_nsec(&identity.nsec)
        .with_context(|| "Failed to create keys from nsec")?;
    println!("✓ Keys are valid");
    println!("  Public key: {}", keys.public_key());

    // Test database connection
    let _cache = cache::EventCache::new(&config.cache.db_path).await
        .with_context(|| "Failed to connect to database")?;
    println!("✓ Database connection successful");

    // Test webhook connectivity
    let webhook = webhook::WebhookDeliverer::new(
        config.webhook.url.clone(),
        config.webhook.dm_url.clone(),
        config.webhook.token.clone(),
        config.webhook.preview_length,
    );

    match webhook.test_webhook().await {
        Ok(()) => println!("✓ Webhook connectivity test passed"),
        Err(e) => {
            println!("⚠ Webhook connectivity test failed: {}", e);
            println!("  (This is non-fatal, the bridge will still work)");
        }
    }

    println!("\nConfiguration test completed successfully!");
    println!("Bridge is ready to run with these settings.");

    Ok(())
}

async fn wait_for_shutdown() -> Result<()> {
    // Wait for either SIGTERM or SIGINT
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
        .context("Failed to install SIGTERM handler")?;
    let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())
        .context("Failed to install SIGINT handler")?;

    tokio::select! {
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM");
        },
        _ = sigint.recv() => {
            tracing::info!("Received SIGINT");
        },
        _ = signal::ctrl_c() => {
            tracing::info!("Received Ctrl+C");
        }
    }

    Ok(())
}

fn init_logging(level: &str) -> Result<()> {
    let filter = match level.to_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "info" => tracing::Level::INFO,
        "debug" => tracing::Level::DEBUG,
        "trace" => tracing::Level::TRACE,
        _ => tracing::Level::INFO,
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter.to_string()))
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_thread_ids(false)
                .with_thread_names(false)
                .compact()
        )
        .init();

    Ok(())
}