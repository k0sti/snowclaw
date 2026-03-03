//! Nostr NIP-29 group messaging integration test.
//!
//! Requires the `nak` binary (available at `/run/current-system/sw/bin/nak`).
//! Run with: `cargo test -- --ignored nostr_integration`

mod common;

use nostr_sdk::prelude::*;
use std::time::Duration;
use tokio::sync::mpsc;
use zeroclaw::channels::nostr::{NostrChannel, NostrChannelConfig};
use zeroclaw::channels::traits::{Channel, SendMessage};

const NAK_BIN: &str = "/run/current-system/sw/bin/nak";
const RELAY_PORT: u16 = 19847;
const RELAY_URL: &str = "ws://127.0.0.1:19847";

/// Start `nak serve` as a background process, returning the child handle.
fn start_nak_relay() -> std::process::Child {
    std::process::Command::new(NAK_BIN)
        .args(["serve", "--port", &RELAY_PORT.to_string(), "--quiet"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("Failed to start nak serve — is nak installed?")
}

/// Wait for the relay to accept TCP connections.
async fn wait_for_relay() {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(format!("127.0.0.1:{RELAY_PORT}"))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("Relay did not start within 5 seconds");
}

#[tokio::test]
#[ignore] // requires nak binary
async fn nostr_integration_group_messaging() {
    // --- Setup ---
    let mut nak = start_nak_relay();
    wait_for_relay().await;

    // Generate keypairs
    let agent_keys = Keys::generate();
    let user_keys = Keys::generate();

    // Create agent's NostrChannel
    let agent_config = NostrChannelConfig {
        relays: vec![RELAY_URL.to_string()],
        keys: agent_keys.clone(),
        groups: vec!["test-group".to_string()],
        listen_dms: false,
        allowed_pubkeys: vec![], // allow all
        respond_mode: zeroclaw::channels::nostr::RespondMode::All,
        group_respond_mode: std::collections::HashMap::new(),
        mention_names: vec!["snowclaw".to_string()],
        owner: None,
        context_history: 20,
        persist_dir: std::path::PathBuf::from("/tmp/snowclaw-test"),
        indexed_paths: Vec::new(),
    };

    let agent_channel = NostrChannel::new(agent_config)
        .await
        .expect("Failed to create agent NostrChannel");

    // Start listening in background
    let (tx, mut rx) = mpsc::channel(32);
    let agent_channel_ref = std::sync::Arc::new(agent_channel);
    let listener_channel = agent_channel_ref.clone();
    let listener_handle = tokio::spawn(async move {
        listener_channel.listen(tx).await
    });

    // Give subscription time to propagate
    tokio::time::sleep(Duration::from_millis(500)).await;

    // --- User sends a kind 9 group message ---
    let user_client = Client::new(user_keys.clone());
    user_client.add_relay(RELAY_URL).await.expect("add relay");
    user_client.connect().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let tags = vec![Tag::custom(TagKind::custom("h"), vec!["test-group".to_string()])];
    let event_builder = EventBuilder::new(Kind::Custom(9), "Hello from user!").tags(tags);
    let send_output = user_client
        .send_event_builder(event_builder)
        .await
        .expect("Failed to send user message");
    let sent_event_id = send_output.val;

    // --- Verify agent receives the message ---
    let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("Timed out waiting for message")
        .expect("Channel closed without message");

    // Verify event ID matches
    assert_eq!(received.id, sent_event_id.to_hex());

    // Verify compact header format: [nostr:group=#test-group from=<name> kind=9 id=<8chars>]
    assert!(
        received.content.starts_with("[nostr:group=#test-group"),
        "Unexpected header format: {}",
        received.content
    );
    assert!(
        received.content.contains("kind=9"),
        "Missing kind in header: {}",
        received.content
    );
    assert!(
        received.content.contains("Hello from user!"),
        "Missing message body: {}",
        received.content
    );

    // Verify channel name
    assert_eq!(received.channel, "nostr:#test-group");

    // Verify reply_target is group-prefixed
    assert!(
        received.reply_target.starts_with('#'),
        "reply_target should start with #: {}",
        received.reply_target
    );

    // --- Verify event cache ---
    let cached = agent_channel_ref
        .get_raw_event(&sent_event_id.to_hex())
        .await;
    assert!(cached.is_some(), "Event should be in cache");
    assert_eq!(cached.unwrap().content, "Hello from user!");

    // --- Test sending a reply back ---
    let reply = SendMessage::new("Hello back from agent!", "#test-group");
    agent_channel_ref
        .send(&reply)
        .await
        .expect("Failed to send reply");

    // Give it a moment to propagate
    tokio::time::sleep(Duration::from_millis(300)).await;

    // --- Cleanup ---
    listener_handle.abort();
    user_client.disconnect().await;
    nak.kill().expect("Failed to kill nak");
    let _ = nak.wait();
}
