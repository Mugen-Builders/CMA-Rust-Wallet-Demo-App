#!/usr/bin/env bash
# Anvil "devnet" setup for the emergency-withdrawal test.
#
# Starts a local anvil with the Cartesi v3 contracts at their CANONICAL addresses, loaded
# from the rollups-contracts release anvil-state tarball — no repo to clone, nothing to
# compile, no soldeer, and no cartesi/rollups-node-devnet image to build. The node stack
# (compose.local.yaml) dials this anvil via the Docker host-gateway.
#
# Usage:
#   ./run_devnet.sh up      # download state (cached), start anvil, deploy the per-token builder
#   ./run_devnet.sh down     # stop anvil
#   ./run_devnet.sh addresses  # print the canonical contract addresses
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
export PATH="$HOME/.foundry/bin:$PATH"

CONTRACTS_VER=3.0.0-alpha.6
FOUNDRY_VER=v1.4.3
TARBALL=rollups-contracts-${CONTRACTS_VER}-anvil-${FOUNDRY_VER}.tar.gz
TARBALL_URL=https://github.com/cartesi/rollups-contracts/releases/download/v${CONTRACTS_VER}/${TARBALL}
RPC=http://localhost:8545
PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
INPUTBOX=0x346B3df038FE9f8380071eC6514D5a83aD143939
TOKEN=0x88A2120B7068E78692C8fd12E751d610B6377E4d
BUILDER_FACTORY=0xdB4EC04a2792A04cF7421f99A70F624681dd8e50
BUILDER=0x0745787835A019cd4dae8EDB541Fbc0647793d63
SALT=0x0000000000000000000000000000000000000000000000000000000000000000

log() { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
die() { printf '\033[1;31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 not found on PATH (install Foundry: curl -L https://foundry.paradigm.xyz | bash && foundryup -i v1.4.3)"; }

up() {
  need anvil; need cast; need curl
  log "Fetch the rollups-contracts anvil-state tarball (cached in anvil-state/)"
  [ -f anvil-state/state.json ] || {
    mkdir -p anvil-state
    curl -sSL -o "anvil-state/${TARBALL}" "$TARBALL_URL" || die "download failed"
    tar xzf "anvil-state/${TARBALL}" -C anvil-state
  }
  [ -f anvil-state/state.json ] || die "state.json missing after extract"

  log "Start anvil (host 0.0.0.0, block-time 1, contracts pre-loaded)"
  if ! cast chain-id --rpc-url "$RPC" >/dev/null 2>&1; then
    nohup anvil --host 0.0.0.0 --block-time 1 --load-state anvil-state/state.json --quiet \
      > anvil.log 2>&1 & echo $! > .anvil.pid; disown || true
    for _ in $(seq 1 20); do cast chain-id --rpc-url "$RPC" >/dev/null 2>&1 && break; sleep 1; done
  fi
  [ "$(cast code "$INPUTBOX" --rpc-url "$RPC")" != "0x" ] || die "InputBox not present in loaded state"

  log "Deploy the per-token withdrawal output builder (idempotent)"
  if [ "$(cast code "$BUILDER" --rpc-url "$RPC")" = "0x" ]; then
    cast send --private-key "$PK" --rpc-url "$RPC" "$BUILDER_FACTORY" \
      'newUsdWithdrawalOutputBuilder(address,bytes32)' "$TOKEN" "$SALT" >/dev/null
  fi

  log "Devnet ready: $RPC (chain 31337)  InputBox=$INPUTBOX  builder=$BUILDER"
}

down() {
  log "Stopping anvil"
  [ -f .anvil.pid ] && kill "$(cat .anvil.pid)" 2>/dev/null || pkill -f "anvil --host" 2>/dev/null || true
  rm -f .anvil.pid
}

addresses() {
  echo "InputBox                      0x346B3df038FE9f8380071eC6514D5a83aD143939"
  echo "ApplicationFactory            0xC549F89cF1ca43eDDECC64Ac2208F4b283B1c483"
  echo "SelfHostedApplicationFactory  0x6145C5996a71a379E030aEb0440df79D60833418"
  echo "AuthorityFactory              0x3C1FE01c542a88A523FF6847eD1E26176c8C4ED0"
  echo "ERC20Portal                   0x22E57511C30CcE6CDaa742E13CE3b774fDC663b1"
  echo "TestFungibleToken             0x88A2120B7068E78692C8fd12E751d610B6377E4d"
  echo "UsdWithdrawalOutputBuilder    0x0745787835A019cd4dae8EDB541Fbc0647793d63"
}

case "${1:-up}" in
  up) up ;;
  down) down ;;
  addresses) addresses ;;
  *) echo "usage: $0 [up|down|addresses]"; exit 1 ;;
esac
