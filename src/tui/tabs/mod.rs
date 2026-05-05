//! Tab identifiers and per-tab state.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabId {
    Config,
    Operate,
    Checkout,
}

impl TabId {
    pub const ALL: [TabId; 3] = [TabId::Config, TabId::Operate, TabId::Checkout];

    pub fn label(self) -> &'static str {
        match self {
            TabId::Config => "1:Config",
            TabId::Operate => "2:Operate",
            TabId::Checkout => "3:Checkout",
        }
    }

    pub fn next(self) -> TabId {
        match self {
            TabId::Config => TabId::Operate,
            TabId::Operate => TabId::Checkout,
            TabId::Checkout => TabId::Config,
        }
    }

    pub fn prev(self) -> TabId {
        match self {
            TabId::Config => TabId::Checkout,
            TabId::Operate => TabId::Config,
            TabId::Checkout => TabId::Operate,
        }
    }
}
