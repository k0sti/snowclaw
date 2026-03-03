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

# Pull upstream and merge (simple)
sync:
    git fetch upstream
    git merge upstream/main

# Rebase onto upstream with squash — see docs/rebase-guide.md
rebase-upstream:
    #!/usr/bin/env bash
    set -euo pipefail
    BRANCH=$(git branch --show-current)
    BASE=$(git merge-base "$BRANCH" upstream/main)
    LOCAL_COUNT=$(git log --oneline "$BASE..$BRANCH" | wc -l)
    echo "📊 $LOCAL_COUNT local commits on top of upstream"
    echo ""
    # Backup
    BACKUP="backup/$BRANCH-pre-rebase-$(date +%Y%m%d-%H%M%S)"
    git branch "$BACKUP"
    echo "💾 Backup: $BACKUP"
    # Fetch upstream
    git fetch upstream
    BEHIND=$(git log --oneline "$BRANCH..upstream/main" | wc -l)
    echo "📥 $BEHIND commits behind upstream/main"
    echo ""
    if [ "$LOCAL_COUNT" -gt 10 ]; then
        echo "⚠️  $LOCAL_COUNT local commits — consider squashing first:"
        echo "   git rebase -i $BASE"
        echo "   Then re-run: just rebase-upstream"
        exit 1
    fi
    echo "🔄 Rebasing onto upstream/main..."
    git rebase upstream/main
    echo ""
    echo "🔨 Running cargo check..."
    cargo check 2>&1 | tail -5
    echo ""
    echo "✅ Rebase complete. Local commits:"
    git log --oneline upstream/main.."$BRANCH"
