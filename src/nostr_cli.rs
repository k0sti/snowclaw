use anyhow::Result;
use clap::Subcommand;
use nostr_sdk::prelude::*;

use crate::config::Config;

#[derive(Subcommand, Debug)]
pub enum NostrCommands {
    /// Generate a new Nostr keypair
    Keygen,
    /// Show current Nostr identity from config
    Whoami,
    /// Import an existing nsec into config
    Import {
        /// Nostr secret key (nsec1... bech32 format)
        nsec: String,
    },
    /// List configured relays
    Relays,
    /// Manage dynamic config via NIP-78 events
    Config {
        #[clap(subcommand)]
        action: NostrConfigAction,
    },
    /// View and manage per-npub/group memory
    Memory {
        #[clap(subcommand)]
        action: NostrMemoryAction,
    },
    /// Publish/update Nostr profile (kind 0 metadata)
    Profile {
        /// Display name
        #[clap(long)]
        name: Option<String>,
        /// About text
        #[clap(long)]
        about: Option<String>,
        /// Profile picture URL
        #[clap(long)]
        picture: Option<String>,
        /// NIP-05 identifier
        #[clap(long)]
        nip05: Option<String>,
    },
    /// Interactive onboarding: generate key, set profile, join groups
    Onboard,
}

#[derive(Subcommand, Debug)]
pub enum NostrConfigAction {
    /// Set config for a group or globally
    Set {
        /// Group name to configure
        #[clap(long)]
        group: Option<String>,
        /// Set global config (instead of per-group)
        #[clap(long)]
        global: bool,
        /// Respond mode: all, mention, owner, none
        #[clap(long)]
        respond_mode: Option<String>,
        /// Number of context history messages
        #[clap(long)]
        context_history: Option<usize>,
    },
    /// Get current dynamic config
    Get {
        /// Group name to query (omit for global)
        #[clap(long)]
        group: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum NostrMemoryAction {
    /// Show memory for an npub (hex or npub1...)
    Show {
        /// Npub or hex pubkey
        npub: String,
    },
    /// Add a owner note about an npub
    Note {
        /// Npub or hex pubkey
        npub: String,
        /// Note text
        text: String,
    },
    /// Show group memory
    Group {
        /// Group ID
        group: String,
    },
    /// List all known contacts
    List,
}

pub async fn handle_command(cmd: NostrCommands, config: &Config) -> Result<()> {
    match cmd {
        NostrCommands::Keygen => cmd_keygen(config),
        NostrCommands::Whoami => cmd_whoami(config),
        NostrCommands::Import { nsec } => cmd_import(nsec, config),
        NostrCommands::Relays => cmd_relays(config),
        NostrCommands::Config { action } => cmd_config(action, config).await,
        NostrCommands::Memory { action } => cmd_memory(action, config).await,
        NostrCommands::Profile { name, about, picture, nip05 } => {
            cmd_profile(name, about, picture, nip05, config).await
        }
        NostrCommands::Onboard => cmd_onboard(config).await,
    }
}

async fn cmd_config(action: NostrConfigAction, config: &Config) -> Result<()> {
    let nostr_cfg = config
        .channels_config
        .nostr
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Nostr not configured"))?;

    let nsec_str = nostr_cfg
        .nsec
        .clone()
        .or_else(|| std::env::var("SNOWCLAW_NSEC").ok())
        .ok_or_else(|| anyhow::anyhow!("No nsec configured"))?;

    let keys = Keys::parse(&nsec_str)?;

    let owner = nostr_cfg
        .owner
        .as_ref()
        .and_then(|s| PublicKey::parse(s).ok());

    let channel_config = crate::channels::nostr::NostrChannelConfig {
        relays: nostr_cfg.relays.clone(),
        keys: keys.clone(),
        groups: nostr_cfg.groups.clone(),
        listen_dms: false,
        allowed_pubkeys: vec![],
        respond_mode: crate::channels::nostr::RespondMode::None,
        group_respond_mode: std::collections::HashMap::new(),
        mention_names: vec![],
        owner,
        context_history: nostr_cfg.context_history,
        persist_dir: config.config_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf(),
        indexed_paths: Vec::new(),
        index_interval_minutes: 30,
    };

    let channel = crate::channels::nostr::NostrChannel::new(channel_config).await?;

    match action {
        NostrConfigAction::Set {
            group,
            global,
            respond_mode,
            context_history,
        } => {
            let d_tag = if global {
                "snowclaw:config:global".to_string()
            } else if let Some(ref g) = group {
                format!("snowclaw:config:group:{g}")
            } else {
                anyhow::bail!("Specify --global or --group <name>");
            };

            let event_id = channel
                .publish_config_event(&d_tag, respond_mode.as_deref(), context_history)
                .await?;

            println!("✅ Published config event: {event_id}");
            if let Some(mode) = &respond_mode {
                println!("   respond_mode: {mode}");
            }
            if let Some(n) = context_history {
                println!("   context_history: {n}");
            }
        }
        NostrConfigAction::Get { group } => {
            let scope = if let Some(ref g) = group {
                format!("group:{g}")
            } else {
                "global".to_string()
            };
            println!("📡 Dynamic config for {scope}:");
            println!("   (Fetched from relay on channel startup — use `set` to publish updates)");
            // Show file config as baseline
            if let Some(ref g) = group {
                if let Some(mode) = nostr_cfg.group_respond_mode.get(g) {
                    println!("   file config respond_mode: {mode}");
                }
            }
            println!("   file config respond_mode (default): {}", nostr_cfg.respond_mode);
            println!("   file config context_history: {}", nostr_cfg.context_history);
        }
    }

    Ok(())
}

fn cmd_keygen(config: &Config) -> Result<()> {
    let keys = Keys::generate();
    let nsec = keys.secret_key().to_bech32()?;
    let npub = keys.public_key().to_bech32()?;
    let hex_pubkey = keys.public_key().to_hex();

    println!("🔑 New Nostr keypair generated:\n");
    println!("  npub: {npub}");
    println!("  nsec: {nsec}");
    println!("  hex:  {hex_pubkey}");
    println!();

    print!("Save to config? [Y/n] ");
    use std::io::Write;
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input.is_empty() || input == "y" || input == "yes" {
        save_nsec_to_config(config, &nsec)?;
        println!("✅ Saved to config.");
    } else {
        println!("Not saved. You can import later with: snowclaw nostr import {nsec}");
    }

    Ok(())
}

fn cmd_whoami(config: &Config) -> Result<()> {
    let nsec_str = get_nsec_from_config(config);

    match nsec_str {
        Some(nsec_str) => {
            let keys = Keys::parse(&nsec_str)
                .map_err(|e| anyhow::anyhow!("Invalid nsec in config: {e}"))?;
            let npub = keys.public_key().to_bech32()?;
            let hex_pubkey = keys.public_key().to_hex();

            println!("🔑 Nostr identity:\n");
            println!("  npub: {npub}");
            println!("  hex:  {hex_pubkey}");
        }
        None => {
            println!("No Nostr identity configured.");
            println!(
                "Run `snowclaw nostr keygen` or `snowclaw nostr import <nsec>` to set one up."
            );
        }
    }

    Ok(())
}

fn cmd_import(nsec: String, config: &Config) -> Result<()> {
    let keys =
        Keys::parse(&nsec).map_err(|e| anyhow::anyhow!("Invalid nsec: {e}"))?;

    let npub = keys.public_key().to_bech32()?;
    let nsec_bech32 = keys.secret_key().to_bech32()?;

    save_nsec_to_config(config, &nsec_bech32)?;

    println!("✅ Imported Nostr identity:\n");
    println!("  npub: {npub}");
    println!("  hex:  {}", keys.public_key().to_hex());

    Ok(())
}

fn cmd_relays(config: &Config) -> Result<()> {
    match &config.channels_config.nostr {
        Some(nostr_cfg) if !nostr_cfg.relays.is_empty() => {
            println!("📡 Configured relays:\n");
            for relay in &nostr_cfg.relays {
                println!("  {relay}");
            }
        }
        _ => {
            println!("No relays configured.");
            println!("Add relays to [channels_config.nostr] relays in config.toml");
        }
    }

    Ok(())
}

fn get_nsec_from_config(config: &Config) -> Option<String> {
    config
        .channels_config
        .nostr
        .as_ref()
        .and_then(|n| n.nsec.clone())
        .or_else(|| std::env::var("SNOWCLAW_NSEC").ok())
}

fn save_nsec_to_config(config: &Config, nsec: &str) -> Result<()> {
    let mut config = config.clone();

    match config.channels_config.nostr.as_mut() {
        Some(nostr_cfg) => {
            nostr_cfg.nsec = Some(nsec.to_string());
        }
        None => {
            config.channels_config.nostr = Some(crate::config::NostrConfig {
                relays: Vec::new(),
                nsec: Some(nsec.to_string()),
                groups: Vec::new(),
                listen_dms: true,
                allowed_pubkeys: Vec::new(),
                respond_mode: "mention".to_string(),
                group_respond_mode: std::collections::HashMap::new(),
                mention_names: Vec::new(),
                owner: None,
                context_history: 20,
            });
        }
    }

    config.save()?;
    Ok(())
}

async fn cmd_memory(action: NostrMemoryAction, config: &Config) -> Result<()> {
    let persist_dir = config.config_path.parent().unwrap_or(std::path::Path::new("."));
    let memory = crate::channels::nostr_memory::NostrMemory::new(persist_dir);

    match action {
        NostrMemoryAction::Show { npub } => {
            let hex = resolve_to_hex(&npub)?;
            match memory.get_npub(&hex).await {
                Some(m) => {
                    println!("📇 Contact: {} ({})", m.display_name, &m.npub_hex[..16]);
                    println!("   First seen: {} {}", m.first_seen, m.first_seen_group.as_deref().unwrap_or("(DM)"));
                    println!("   Last interaction: {}", m.last_interaction);
                    if !m.name_history.is_empty() {
                        println!("   Name history:");
                        for (ts, name) in &m.name_history {
                            println!("     {ts}: {name}");
                        }
                    }
                    if !m.owner_notes.is_empty() {
                        println!("   Owner notes:");
                        for n in &m.owner_notes {
                            println!("     - {n}");
                        }
                    }
                    if !m.notes.is_empty() {
                        println!("   Agent notes:");
                        for n in &m.notes {
                            println!("     - {n}");
                        }
                    }
                }
                None => println!("No memory found for {}", &hex[..16]),
            }
        }
        NostrMemoryAction::Note { npub, text } => {
            let hex = resolve_to_hex(&npub)?;
            // Ensure entry exists
            memory.ensure_npub(&hex, "unknown", chrono::Utc::now().timestamp() as u64, None, false).await;
            memory.add_npub_owner_note(&hex, &text).await;
            memory.force_flush().await;
            println!("✅ Owner note added for {}", &hex[..16]);
        }
        NostrMemoryAction::Group { group } => {
            match memory.get_group(&group).await {
                Some(g) => {
                    println!("📋 Group: #{}", g.group_id);
                    if let Some(ref p) = g.purpose {
                        println!("   Purpose: {p}");
                    }
                    println!("   Members seen: {}", g.members_seen.len());
                    println!("   Last activity: {}", g.last_activity);
                    if !g.notes.is_empty() {
                        println!("   Notes:");
                        for n in &g.notes {
                            println!("     - {n}");
                        }
                    }
                }
                None => println!("No memory found for group #{group}"),
            }
        }
        NostrMemoryAction::List => {
            let npubs = memory.list_npubs().await;
            if npubs.is_empty() {
                println!("No contacts in memory yet.");
            } else {
                println!("📇 Known contacts ({}):", npubs.len());
                let mut sorted = npubs;
                sorted.sort_by(|a, b| b.last_interaction.cmp(&a.last_interaction));
                for m in sorted {
                    let notes_count = m.notes.len() + m.owner_notes.len();
                    let notes_tag = if notes_count > 0 { format!(" [{notes_count} notes]") } else { String::new() };
                    println!("  {} ({:.16}){}", m.display_name, m.npub_hex, notes_tag);
                }
            }
        }
    }

    Ok(())
}

fn resolve_to_hex(input: &str) -> Result<String> {
    if input.starts_with("npub1") {
        let pk = nostr_sdk::PublicKey::from_bech32(input)
            .map_err(|e| anyhow::anyhow!("Invalid npub: {e}"))?;
        Ok(pk.to_hex())
    } else if input.len() == 64 && input.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(input.to_lowercase())
    } else {
        anyhow::bail!("Expected npub1... or 64-char hex pubkey, got: {input}")
    }
}

/// Publish or update Nostr profile (kind 0).
async fn cmd_profile(
    name: Option<String>,
    about: Option<String>,
    picture: Option<String>,
    nip05: Option<String>,
    config: &Config,
) -> Result<()> {
    let nostr_cfg = config
        .channels_config
        .nostr
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No [channels_config.nostr] in config"))?;

    let nsec_str = nostr_cfg
        .nsec
        .clone()
        .or_else(|| std::env::var("SNOWCLAW_NSEC").ok())
        .ok_or_else(|| anyhow::anyhow!("No nsec configured"))?;

    let keys = nostr_sdk::Keys::parse(&nsec_str)?;
    let client = nostr_sdk::Client::new(keys.clone());

    for relay in &nostr_cfg.relays {
        client.add_relay(relay.as_str()).await?;
    }
    client.connect().await;

    // Build metadata JSON
    let mut metadata = nostr_sdk::Metadata::new();
    if let Some(n) = &name {
        metadata = metadata.name(n);
        metadata = metadata.display_name(n);
    }
    if let Some(a) = &about {
        metadata = metadata.about(a);
    }
    if let Some(p) = &picture {
        metadata = metadata.picture(nostr_sdk::Url::parse(p)?);
    }
    if let Some(nip) = &nip05 {
        metadata = metadata.nip05(nip);
    }

    // Build metadata event with NIP-AE bot tag
    let mut tags = vec![
        Tag::custom(TagKind::Custom("bot".into()), Vec::<String>::new()),
    ];

    // If owner configured, add p-tag pointing to owner
    if let Some(owner_str) = &nostr_cfg.owner {
        let owner_pk = nostr_sdk::PublicKey::parse(owner_str)?;
        tags.push(Tag::public_key(owner_pk));
    }

    let builder = nostr_sdk::EventBuilder::metadata(&metadata).tags(tags);
    let output = client.send_event_builder(builder).await?;
    let npub = keys.public_key().to_bech32()?;

    println!("✅ Profile published!");
    println!("   npub: {npub}");
    if let Some(n) = &name { println!("   name: {n}"); }
    if let Some(a) = &about { println!("   about: {a}"); }
    if let Some(p) = &picture { println!("   picture: {p}"); }
    if let Some(nip) = &nip05 { println!("   nip05: {nip}"); }
    println!("   event: {}", output.val);

    client.disconnect().await;
    Ok(())
}

/// Interactive onboarding wizard.
async fn cmd_onboard(config: &Config) -> Result<()> {
    use std::io::{self, Write};

    println!("🏔️  Snowclaw Nostr Onboarding");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();

    // Step 1: Check for existing key
    let has_key = config
        .channels_config
        .nostr
        .as_ref()
        .and_then(|n| n.nsec.as_ref())
        .is_some()
        || std::env::var("SNOWCLAW_NSEC").is_ok();

    if has_key {
        cmd_whoami(config)?;
        println!();
    } else {
        println!("No Nostr key found. Generating one...");
        cmd_keygen(config)?;
        println!();
        println!("⚠️  Save your nsec! It's stored in:");
        println!("   ~/.snowclaw/config.toml");
        println!("   Back it up to ~/.snowclaw/secrets/nostr.json");
        println!();
    }

    // Step 2: Set profile
    print!("Display name [Snowclaw❄️]: ");
    io::stdout().flush()?;
    let mut name_input = String::new();
    io::stdin().read_line(&mut name_input)?;
    let name = if name_input.trim().is_empty() {
        "Snowclaw❄️".to_string()
    } else {
        name_input.trim().to_string()
    };

    print!("About [Nostr-native AI agent]: ");
    io::stdout().flush()?;
    let mut about_input = String::new();
    io::stdin().read_line(&mut about_input)?;
    let about = if about_input.trim().is_empty() {
        "Nostr-native AI agent 🏔️".to_string()
    } else {
        about_input.trim().to_string()
    };

    print!("Profile picture URL (optional): ");
    io::stdout().flush()?;
    let mut pic_input = String::new();
    io::stdin().read_line(&mut pic_input)?;
    let picture = if pic_input.trim().is_empty() {
        None
    } else {
        Some(pic_input.trim().to_string())
    };

    println!();
    println!("Publishing profile...");
    cmd_profile(Some(name), Some(about), picture, None, config).await?;

    // Step 3: Show relay status
    println!();
    cmd_relays(config)?;

    println!();
    println!("🎉 Onboarding complete! Start the daemon with: snowclaw daemon");

    Ok(())
}
