# Energy Query Function Testing Guide

## Feature Overview

I have added energy query functionality to the Terminos blockchain wallet, including:

1. **Daemon API**: Added `get_energy` RPC method
2. **Wallet API**: Added `get_energy` RPC method
3. **Wallet CLI**: Added `energy` interactive command

## Feature Characteristics

### Query information includes:
- **Frozen TOS Amount** (frozen_tos): Amount of TOS frozen by user to obtain energy
- **Total Energy** (total_energy): Total energy units owned by user
- **Used Energy** (used_energy): Energy units already consumed
- **Available Energy** (available_energy): Currently available energy units
- **Last Update** (last_update): Topoheight when energy information was last updated

## Testing Steps

### 1. Start Daemon
```bash
cd /Users/tomisetsu/tos-network/terminos
cargo run --bin terminos_daemon
```

### 2. Start Wallet and Connect to Daemon
```bash
cargo run --bin terminos_wallet
```

In the wallet, execute:
```
online_mode
```

### 3. Test Energy Query Command
In the wallet interactive interface, execute:
```
energy
```

### 4. Test RPC API
You can call RPC methods via HTTP or WebSocket:

**HTTP POST request:**
```bash
curl -X POST http://localhost:8080/json_rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "get_energy",
    "params": {
      "address": "YOUR_WALLET_ADDRESS"
    }
  }'
```

**Expected response:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "frozen_tos": 1000,
    "total_energy": 1000,
    "used_energy": 50,
    "available_energy": 950,
    "last_update": 12345
  }
}
```

## Implementation Details

### Daemon Implementation
- File: `daemon/src/rpc/rpc.rs`
- Method: `get_energy`
- Parameters: `GetEnergyParams` (including address)
- Response: `GetEnergyResult` (including all energy information)

### Wallet Implementation
- File: `wallet/src/api/rpc.rs`
- Method: `get_energy`
- Function: Call daemon's get_energy method and return results

### CLI Command Implementation
- File: `wallet/src/main.rs`
- Command: `energy`
- Function: Display formatted energy information

## Important Notes

1. **Online Mode Required**: Energy query functionality requires wallet to be in online mode
2. **Network Connection**: Must be connected to daemon to get energy information
3. **Error Handling**: If account has no energy information, default values (all zeros) will be returned

## Future Features

Future features that could be added:
1. Energy freeze/unfreeze commands
2. Energy usage history queries
3. Energy leasing functionality
4. Energy price queries 