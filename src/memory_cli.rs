//! CLI subcommands for Nostr memory management.
//!
//! Provides `snowclaw memory show <key>`, `memory list`, `memory note <key> <content>`
//! using the configured memory backend.

use anyhow::Result;
use clap::Subcommand;

use crate::config::Config;
use crate::memory::{self, MemoryCategory};

#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    /// Show a specific memory entry by key
    Show {
        /// Memory key to look up
        key: String,
    },
    /// List all memory entries
    List {
        /// Filter by category: core, daily, conversation
        #[arg(short, long)]
        category: Option<String>,
    },
    /// Store a note in memory
    Note {
        /// Memory key
        key: String,
        /// Note content
        content: String,
        /// Category: core, daily, conversation (default: core)
        #[arg(short = 'C', long, default_value = "core")]
        category: String,
    },
    /// Forget (delete) a memory entry
    Forget {
        /// Memory key to remove
        key: String,
    },
    /// Count total memory entries
    Count,
}

fn parse_category(s: &str) -> MemoryCategory {
    match s.to_lowercase().as_str() {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

pub async fn handle_command(cmd: MemoryCommands, config: &Config) -> Result<()> {
    let mem = memory::create_memory(&config.memory, &config.workspace_dir, None)?;

    match cmd {
        MemoryCommands::Show { key } => {
            match mem.get(&key).await? {
                Some(entry) => {
                    println!("Key:      {}", entry.key);
                    println!("Category: {}", entry.category);
                    println!("Updated:  {}", entry.timestamp);
                    if let Some(sid) = &entry.session_id {
                        println!("Session:  {sid}");
                    }
                    println!("---");
                    println!("{}", entry.content);
                }
                None => {
                    println!("No memory found for key: {key}");
                }
            }
        }

        MemoryCommands::List { category } => {
            let cat = category.as_deref().map(parse_category);
            let entries = mem.list(cat.as_ref(), None).await?;

            if entries.is_empty() {
                println!("No memory entries found.");
                return Ok(());
            }

            println!("{} memory entries:", entries.len());
            let mut sorted = entries;
            sorted.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            for entry in &sorted {
                let preview: String = entry.content.chars().take(60).collect();
                let ellipsis = if entry.content.len() > 60 { "..." } else { "" };
                println!(
                    "  [{}] {}: {}{} ({})",
                    entry.category, entry.key, preview, ellipsis, entry.timestamp
                );
            }
        }

        MemoryCommands::Note {
            key,
            content,
            category,
        } => {
            let cat = parse_category(&category);
            mem.store(&key, &content, cat, None).await?;
            println!("Stored memory: {key} ({category})");
        }

        MemoryCommands::Forget { key } => {
            if mem.forget(&key).await? {
                println!("Forgot memory: {key}");
            } else {
                println!("No memory found for key: {key}");
            }
        }

        MemoryCommands::Count => {
            let count = mem.count().await?;
            println!("{count} memory entries");
        }
    }

    Ok(())
}
