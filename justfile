# Snowclaw — Nostr-native AI agent

# Build release binary
build:
    cargo build --release

# Run tests
test:
    cargo test --lib

# Run integration tests (requires nak)
test-integration:
    cargo test --test nostr_integration -- --ignored

# Build, deploy binary, and restart service
deploy: build
    sudo systemctl restart snowclaw
    @echo "✅ Snowclaw deployed and restarted"
    @sleep 2
    @systemctl status snowclaw --no-pager | head -15

# Restart service only
restart:
    sudo systemctl restart snowclaw

# Show service status and recent logs
status:
    @systemctl status snowclaw --no-pager | head -15
    @echo "---"
    @journalctl -u snowclaw --no-pager -n 20

# Follow live logs
logs:
    journalctl -u snowclaw -f

# Stop service
stop:
    sudo systemctl stop snowclaw

# Generate a new Nostr keypair
keygen:
    cargo run -- nostr keygen

# Show current Nostr identity
whoami:
    cargo run -- nostr whoami

# Check config
config:
    @cat ~/.snowclaw/config.toml

# Build and deploy the Nostr bridge
deploy-bridge:
    cargo build --release -p nostr-bridge
    mkdir -p ~/.local/share/nostr-bridge
    sudo nixos-rebuild switch --flake ~/nix-config#studio
    @echo "✅ Nostr bridge deployed and service restarted"
    @sleep 2
    @systemctl status nostr-bridge --no-pager | head -15

# Bridge logs
bridge-logs:
    journalctl -u nostr-bridge -f

# Bridge status
bridge-status:
    @systemctl status nostr-bridge --no-pager | head -15
    @echo "---"
    @journalctl -u nostr-bridge --no-pager -n 20

# Pull upstream and merge
sync:
    git fetch upstream
    git merge upstream/main
