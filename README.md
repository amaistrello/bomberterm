# 💣 BomberTerm

A multiplayer Bomberman game that runs entirely in your terminal. Built with Rust, Tokio, and Ratatui.

![Rust](https://img.shields.io/badge/rust-1.75+-orange?style=flat-square&logo=rust)
![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)

```
┌─────────────────────────────────────────┬──────────────┐
│ ██████████████████████████████████████  │  PLAYERS     │
│ ██  A           ▒▒  ▒▒  ██  ▒▒      ██  │  ────────    │
│ ██  ██  ██  ██  ██  ██  ██  ██  ██  ██  │  0 ♥ Alice   │
│ ██      ▒▒      ●       ▒▒      ▒▒  ██  │    bombs:1/2 │
│ ██  ██  ██  ██  ██  ██  ██  ██  ██  ██  │    range: 3  │
│ ██  ▒▒      ✸   ✸   ✸       ▒▒      ██  │    spd: 2    │
│ ██  ██  ██  ██  ██  ██  ██  ██  ██  ██  │              │
│ ██              ▒▒      ✚           ██  │  1 ♥ Bob     │
│ ██████████████████████████████████████  │    bombs:1/1 │
│                                         │  Tick: 312   │
└─────────────────────────────────────────┴──────────────┘
│ ↑↓←→  Move    Space  Bomb    Q  Quit                   │
└────────────────────────────────────────────────────────┘
```

## Features

- **Fully terminal-based** — no GUI required, runs over SSH
- **LAN multiplayer** — up to 8 players on the same network
- **Host or join** — spin up a server in one keystroke, browse LAN games automatically
- **UDP server discovery** — games appear instantly in the server browser, no IP needed
- **Ready-up lobby** — everyone confirms before the game starts
- **Classic Bomberman mechanics** — bombs, chain reactions, destructible blocks
- **Powerups** — Extra Bomb (`✚`), Longer Range (`◈`), Speed (`»`)
- **Auto rematch** — returns to lobby after game over
- **Single binary** — client and server ship together

## Installation

### Prerequisites

- Rust 1.75 or later

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Build from source

```bash
git clone https://github.com/amaistrello/bomberterm
cd bomberterm
cargo build
```

## Usage

### Host a game

```bash
cargo run -p client
# Select "Host a game" → enter your name → wait in lobby
```

Hosting spins up the game server automatically in the background. Share your local IP with friends so they can join.

### Join a game

```bash
cargo run -p client
# Select "Join a game" → games on your LAN appear automatically
# Select one → enter your name → wait in lobby
```

If automatic discovery doesn't work (e.g. different subnets), press `M` on the server browser to enter an IP manually.

### Start the game

Once everyone has joined the lobby, each player presses `R` to ready up. The game starts the moment all connected players are ready.

## Controls

| Key | Action |
|-----|--------|
| `↑ ↓ ← →` or `W A S D` | Move |
| `Space` | Place bomb |
| `R` | Ready up (lobby) |
| `Q` | Quit / return to menu |
| `M` | Manual IP entry (server browser) |

## Gameplay

### Objective
Be the last player standing.

### Map
A 25×21 grid of hard walls, destructible blocks, and open corridors. Hard walls (`██`) are indestructible. Destructible blocks (`▒▒`) can be blown up.

### Bombs
Press `Space` to place a bomb at your current position. It detonates after 3 seconds, sending explosions (`✸`) in four directions. The color of the bomb changes as the timer runs out:

- 🟢 Green — more than 2 seconds remaining  
- 🟡 Yellow — under 2 seconds  
- 🔴 Red — about to explode  

Chain reactions are supported — if an explosion hits another bomb, it detonates immediately.

### Powerups
Destroying blocks has a 35% chance of dropping a powerup:

| Symbol | Color | Effect |
|--------|-------|--------|
| `✚` | Cyan | Extra Bomb — place one more bomb simultaneously |
| `◈` | Magenta | Longer Range — explosions reach one tile further |
| `»` | Green | Speed — move faster |

Walk over a powerup to collect it. Stats are shown in the sidebar.

### Players
Up to 8 players, each with a distinct color and a protected spawn corridor in the corners and edges of the map.

| ID | Color |
|----|-------|
| 0 | Cyan |
| 1 | Magenta |
| 2 | Yellow |
| 3 | Green |
| 4 | Red |
| 5 | Blue |
| 6 | Light Cyan |
| 7 | Light Magenta |

## Architecture

BomberTerm is a Cargo workspace with three crates:

```
bomberterm/
├── Cargo.toml              # workspace root
└── crates/
    ├── common/             # shared types, map, protocol
    ├── server/             # authoritative game logic (lib + bin)
    └── client/             # TUI renderer, input, networking
```

### Common
Defines all shared types: `Player`, `Bomb`, `Explosion`, `Powerup`, `Map`, and the network protocol (`ClientMsg`, `ServerMsg`, `GameSnapshot`). Serialized with `bincode` for compact wire encoding.

### Server
Runs an authoritative game loop at 100ms ticks (10 ticks/second). Handles:
- TCP connections with `LengthDelimitedCodec` framing
- Shared game state behind `Arc<Mutex<SharedState>>`
- Input processing, movement, bomb ticking, explosion raycasting, powerup drops
- UDP beacon broadcasting for LAN discovery
- Win condition, rematch, lobby phase

Exposed as both a library (`server::run(config)`) and a standalone binary.

### Client
Two concurrent async tasks:
- **Network task** — connects via TCP, sends inputs, pushes snapshots to a `watch` channel
- **Render loop** — polls keyboard events, renders at ~20fps, reads from the watch channel

Screen state machine:
```
MainMenu → ServerBrowser → EnterName → Lobby → InGame → GameOver → Lobby → ...
                ↓
           ManualIpEntry
```

### Networking
- **TCP** — reliable, ordered, framed with a 4-byte length prefix
- **UDP broadcast** — server beacons on port 7778 every 2 seconds for auto-discovery
- **`tokio::select!`** — each connection task fans out snapshots and fans in inputs concurrently

## Development

### Run in development

```bash
# Terminal 1 — host
cargo run -p client

# Terminal 2 — join
cargo run -p client
```

### Run tests

```bash
cargo test
```

### Logs

The client writes logs to `/tmp/bomberterm.log` to avoid corrupting the TUI:

```bash
tail -f /tmp/bomberterm.log
```

### Run the server standalone

```bash
cargo run -p server -- --port 7777
```

## Stack

| Crate | Purpose |
|-------|---------|
| `tokio` | Async runtime, TCP/UDP |
| `tokio-util` | `LengthDelimitedCodec` framing |
| `tokio-serde` | Typed message streams over framed transports |
| `ratatui` | TUI rendering |
| `crossterm` | Terminal backend, keyboard events |
| `serde` + `bincode` | Serialization |
| `socket2` | `SO_REUSEPORT` for UDP discovery |
| `tracing` | Structured logging |

## License

MIT
