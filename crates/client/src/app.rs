use std::net::SocketAddr;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct DiscoveredServer {
    pub game_name: String,
    pub addr: String,
    pub players_current: u8,
    pub players_max: u8,
    pub last_seen: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    MainMenu { cursor: usize },
    EnterName { input: String, mode: NameMode },
    ServerBrowser { cursor: usize },
    ManualIp { input: String },
    Connecting,
    InGame,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NameMode {
    Host,
    Join { addr: String },
}

impl Screen {
    pub fn main_menu() -> Self {
        Screen::MainMenu { cursor: 0 }
    }
}

pub const MENU_ITEMS: &[&str] = &["Host a game", "Join a game", "Quit"];