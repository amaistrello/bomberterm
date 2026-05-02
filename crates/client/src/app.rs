// Every screen the client can be on
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    MainMenu { cursor: usize },
    EnterName { input: String, mode: NameMode },
    Connecting,
    InGame,
}

// Are we hosting or joining?
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