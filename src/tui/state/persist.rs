//! Persisted TUI state schema (per docs/tui_reconstruct_plan.md §16.1).
//!
//! Phase 1b lands the struct only — load/save wiring lands in Phase 2 (AD-8).
//!
//! All fields carry `#[serde(default)]` so additive schema changes are
//! backwards-compatible; rename/remeaning still requires a `schema_version`
//! migration (§16.2 schema-version note).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiPersistedState {
    pub tui_state: TuiSection,
    pub target_filter: TargetFilterState,
    pub operate: OperateState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiSection {
    pub active_tab: ActiveTab,
}

impl Default for TuiSection {
    fn default() -> Self {
        Self {
            active_tab: ActiveTab::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveTab {
    Config,
    Operate,
    Checkout,
}

impl Default for ActiveTab {
    fn default() -> Self {
        ActiveTab::Checkout
    }
}

impl ActiveTab {
    pub fn from_tab_id(t: crate::tui::tabs::TabId) -> Self {
        match t {
            crate::tui::tabs::TabId::Config => ActiveTab::Config,
            crate::tui::tabs::TabId::Operate => ActiveTab::Operate,
            crate::tui::tabs::TabId::Checkout => ActiveTab::Checkout,
        }
    }

    pub fn to_tab_id(self) -> crate::tui::tabs::TabId {
        match self {
            ActiveTab::Config => crate::tui::tabs::TabId::Config,
            ActiveTab::Operate => crate::tui::tabs::TabId::Operate,
            ActiveTab::Checkout => crate::tui::tabs::TabId::Checkout,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetFilterState {
    pub mode: TargetFilterMode,
    pub groups: Vec<String>,
    pub hosts: Vec<String>,
    pub shell: ShellMode,
    pub serial: bool,
    pub timeout: u64,
}

impl Default for TargetFilterState {
    fn default() -> Self {
        Self {
            mode: TargetFilterMode::default(),
            groups: Vec::new(),
            hosts: Vec::new(),
            shell: ShellMode::default(),
            serial: false,
            timeout: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetFilterMode {
    All,
    Groups,
    Hosts,
    Shell,
}

impl Default for TargetFilterMode {
    fn default() -> Self {
        TargetFilterMode::All
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellMode {
    Sh,
    PowerShell,
    Cmd,
}

impl Default for ShellMode {
    fn default() -> Self {
        ShellMode::Sh
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OperateState {
    pub operation: OperationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationKind {
    Check,
    // Run/Exec/Sync land in Phases 5–6.
}

impl Default for OperationKind {
    fn default() -> Self {
        OperationKind::Check
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_loads_as_default() {
        let s: TuiPersistedState = toml::from_str("").unwrap();
        assert_eq!(s.tui_state.active_tab, ActiveTab::Checkout);
        assert_eq!(s.target_filter.mode, TargetFilterMode::All);
        assert_eq!(s.operate.operation, OperationKind::Check);
    }

    #[test]
    fn round_trip_preserves_values() {
        let mut s = TuiPersistedState::default();
        s.tui_state.active_tab = ActiveTab::Operate;
        s.target_filter.mode = TargetFilterMode::Groups;
        s.target_filter.groups = vec!["web".to_string(), "db".to_string()];
        s.target_filter.timeout = 90;
        let serialized = toml::to_string(&s).unwrap();
        let parsed: TuiPersistedState = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed.tui_state.active_tab, ActiveTab::Operate);
        assert_eq!(parsed.target_filter.mode, TargetFilterMode::Groups);
        assert_eq!(parsed.target_filter.groups, vec!["web", "db"]);
        assert_eq!(parsed.target_filter.timeout, 90);
    }

    #[test]
    fn missing_keys_load_as_defaults() {
        let toml_str = r#"
[tui_state]
active_tab = "Config"
"#;
        let s: TuiPersistedState = toml::from_str(toml_str).unwrap();
        assert_eq!(s.tui_state.active_tab, ActiveTab::Config);
        // Missing target_filter table → defaults.
        assert_eq!(s.target_filter.mode, TargetFilterMode::All);
        assert_eq!(s.operate.operation, OperationKind::Check);
    }
}
