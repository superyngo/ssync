//! Focus model + adaptive arrow navigation (per docs/tui_reconstruct_plan.md
//! §8.2 / §8.3 / §8.6).
//!
//! - Arrow keys drive cross-level transitions via `escape_to_parent`.
//! - Tab / Shift+Tab cycle peers within the **current level only** and never
//!   escape level boundaries.
//! - Each focusable component declares its `AxisFreedom` and reports
//!   `at_boundary(dir)` so the dispatch table in `Focusable::handle_arrow`
//!   can decide between "move within element" and "escape to parent".

#![allow(dead_code)]

use super::tabs::TabId;

/// Direction of an arrow keypress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    pub fn axis(self) -> Axis {
        match self {
            Direction::Up | Direction::Down => Axis::Y,
            Direction::Left | Direction::Right => Axis::X,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
}

/// What kinds of arrow keys a focused element absorbs internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisFreedom {
    /// No internal movement — any arrow escapes immediately.
    None,
    /// Only ↑↓ move internally (e.g. vertical list).
    Y,
    /// Only ←→ move internally (e.g. horizontal radio row).
    X,
    /// Both axes move internally (e.g. grid).
    XY,
}

impl AxisFreedom {
    /// Does this element absorb the given direction internally (when not at
    /// a boundary)? If false, the keypress always escapes.
    pub fn absorbs(self, dir: Direction) -> bool {
        match self {
            AxisFreedom::None => false,
            AxisFreedom::Y => matches!(dir, Direction::Up | Direction::Down),
            AxisFreedom::X => matches!(dir, Direction::Left | Direction::Right),
            AxisFreedom::XY => true,
        }
    }
}

/// Per-tab logical zones (per §8.6). Zone IDs map to render layout.
///
/// MVP scope: only the zones needed by Phase 1a's tabs are populated.
/// Later phases extend this enum (Operate sub-zones in Phase 3, Config
/// sub-zones in Phase 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusZone {
    // Checkout tab
    CheckoutControls,
    CheckoutHostTable,
    // Operate tab placeholder
    OperatePlaceholder,
    // Config tab placeholder
    ConfigPlaceholder,
}

impl FocusZone {
    pub fn for_tab(tab: TabId) -> FocusZone {
        match tab {
            TabId::Config => FocusZone::ConfigPlaceholder,
            TabId::Operate => FocusZone::OperatePlaceholder,
            TabId::Checkout => FocusZone::CheckoutHostTable,
        }
    }

    /// Human-readable label for breadcrumb display.
    pub fn label(self) -> &'static str {
        match self {
            FocusZone::CheckoutControls => "Controls",
            FocusZone::CheckoutHostTable => "Rows",
            FocusZone::OperatePlaceholder => "(placeholder)",
            FocusZone::ConfigPlaceholder => "(placeholder)",
        }
    }
}

/// Outcome of an `escape_to_parent` resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeOutcome {
    /// Move to a sibling zone within the same tab.
    SwitchZone(FocusZone),
    /// Wrap at L0 (tab bar) — change the active tab.
    SwitchTab(TabId),
    /// No movement — boundary is sealed (popup root, side edge of zone table).
    Stop,
}

/// Resolve an escape from `from` zone in `dir`, given the current active tab.
///
/// Implements the §8.6 zone neighbour tables. Tab-bar wrap (L0) is handled
/// elsewhere (Tab/Shift+Tab paths); this function only handles arrow-driven
/// zone transitions.
pub fn escape_to_parent(tab: TabId, from: FocusZone, dir: Direction) -> EscapeOutcome {
    match (tab, from, dir) {
        // Checkout tab: Controls ↔ HostTable on ↑↓.
        (TabId::Checkout, FocusZone::CheckoutControls, Direction::Down) => {
            EscapeOutcome::SwitchZone(FocusZone::CheckoutHostTable)
        }
        (TabId::Checkout, FocusZone::CheckoutHostTable, Direction::Up) => {
            EscapeOutcome::SwitchZone(FocusZone::CheckoutControls)
        }
        // All other arrows in Checkout zones are sealed.
        // Placeholder tabs have no sub-zones to cross to.
        _ => EscapeOutcome::Stop,
    }
}

/// Trait every focusable component implements (per §8.3).
///
/// Default `handle_arrow` provides the standard adaptive escape decision:
/// when the element absorbs the direction and is not at a boundary, the
/// component itself moves and returns `Consumed`; otherwise the arrow
/// escapes via `Escaped(dir)`.
pub trait Focusable {
    fn axis_freedom(&self) -> AxisFreedom;

    /// True iff a further move in `dir` would push the cursor off the end
    /// of the absorbing range. Direction must match `axis_freedom`'s axis;
    /// callers should not pass directions the element does not absorb.
    fn at_boundary(&self, dir: Direction) -> bool;

    fn handle_arrow(&mut self, dir: Direction) -> ArrowResult {
        let af = self.axis_freedom();
        if !af.absorbs(dir) {
            return ArrowResult::Escaped(dir);
        }
        if self.at_boundary(dir) {
            return ArrowResult::Escaped(dir);
        }
        self.move_within(dir);
        ArrowResult::Consumed
    }

    /// Called by `handle_arrow` when the element should advance internally.
    fn move_within(&mut self, dir: Direction);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowResult {
    Consumed,
    Escaped(Direction),
}

/// `FocusPath` ties the active focus zone to its breadcrumb trail
/// (per §6.2). Breadcrumb updates only on zone change, never on plain ↑↓.
#[derive(Debug, Clone)]
pub struct FocusPath {
    pub zone: FocusZone,
    pub breadcrumb: Vec<String>,
}

impl FocusPath {
    pub fn for_tab(tab: TabId) -> Self {
        let zone = FocusZone::for_tab(tab);
        let breadcrumb = vec![tab.label().to_string(), zone.label().to_string()];
        Self { zone, breadcrumb }
    }

    pub fn switch_zone(&mut self, tab: TabId, zone: FocusZone) {
        self.zone = zone;
        self.breadcrumb = vec![tab.label().to_string(), zone.label().to_string()];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::components::viewport::Viewport;

    /// Adapter so `Viewport` can be tested through the `Focusable` trait.
    struct ListAdapter<'a>(&'a mut Viewport);

    impl Focusable for ListAdapter<'_> {
        fn axis_freedom(&self) -> AxisFreedom {
            AxisFreedom::Y
        }
        fn at_boundary(&self, dir: Direction) -> bool {
            match dir {
                Direction::Up => self.0.at_top(),
                Direction::Down => self.0.at_bottom(),
                _ => true,
            }
        }
        fn move_within(&mut self, dir: Direction) {
            match dir {
                Direction::Up => self.0.move_up(),
                Direction::Down => self.0.move_down(),
                _ => {}
            }
        }
    }

    #[test]
    fn y_only_horizontal_arrow_escapes_immediately() {
        let mut vp = Viewport::new();
        vp.set_dims(10, 5);
        let mut a = ListAdapter(&mut vp);
        assert_eq!(
            a.handle_arrow(Direction::Left),
            ArrowResult::Escaped(Direction::Left)
        );
        assert_eq!(
            a.handle_arrow(Direction::Right),
            ArrowResult::Escaped(Direction::Right)
        );
    }

    #[test]
    fn y_only_at_boundary_escapes() {
        let mut vp = Viewport::new();
        vp.set_dims(3, 5);
        let mut a = ListAdapter(&mut vp);
        // At top.
        assert_eq!(
            a.handle_arrow(Direction::Up),
            ArrowResult::Escaped(Direction::Up)
        );
    }

    #[test]
    fn y_only_in_middle_consumes() {
        let mut vp = Viewport::new();
        vp.set_dims(10, 5);
        vp.move_down();
        vp.move_down();
        let mut a = ListAdapter(&mut vp);
        assert_eq!(a.handle_arrow(Direction::Up), ArrowResult::Consumed);
        assert_eq!(a.handle_arrow(Direction::Down), ArrowResult::Consumed);
    }

    #[test]
    fn empty_list_is_at_both_boundaries() {
        let mut vp = Viewport::new();
        vp.set_dims(0, 5);
        let mut a = ListAdapter(&mut vp);
        assert_eq!(
            a.handle_arrow(Direction::Up),
            ArrowResult::Escaped(Direction::Up)
        );
        assert_eq!(
            a.handle_arrow(Direction::Down),
            ArrowResult::Escaped(Direction::Down)
        );
    }

    /// X-only adapter for testing radio-row escape.
    struct RadioAdapter {
        index: usize,
        max: usize,
    }
    impl Focusable for RadioAdapter {
        fn axis_freedom(&self) -> AxisFreedom {
            AxisFreedom::X
        }
        fn at_boundary(&self, dir: Direction) -> bool {
            match dir {
                Direction::Left => self.index == 0,
                Direction::Right => self.index + 1 >= self.max,
                _ => true,
            }
        }
        fn move_within(&mut self, dir: Direction) {
            match dir {
                Direction::Left if self.index > 0 => self.index -= 1,
                Direction::Right if self.index + 1 < self.max => self.index += 1,
                _ => {}
            }
        }
    }

    #[test]
    fn x_only_radio_at_first_left_escapes() {
        let mut r = RadioAdapter { index: 0, max: 3 };
        assert_eq!(
            r.handle_arrow(Direction::Left),
            ArrowResult::Escaped(Direction::Left)
        );
    }

    #[test]
    fn x_only_radio_in_middle_consumes() {
        let mut r = RadioAdapter { index: 1, max: 3 };
        assert_eq!(r.handle_arrow(Direction::Left), ArrowResult::Consumed);
        assert_eq!(r.index, 0);
    }

    #[test]
    fn x_only_vertical_arrow_escapes() {
        let mut r = RadioAdapter { index: 1, max: 3 };
        assert_eq!(
            r.handle_arrow(Direction::Up),
            ArrowResult::Escaped(Direction::Up)
        );
    }

    #[test]
    fn escape_to_parent_checkout_table() {
        // Checkout tab: HostTable + Up → Controls; HostTable + Down → Stop.
        assert_eq!(
            escape_to_parent(TabId::Checkout, FocusZone::CheckoutHostTable, Direction::Up),
            EscapeOutcome::SwitchZone(FocusZone::CheckoutControls),
        );
        assert_eq!(
            escape_to_parent(
                TabId::Checkout,
                FocusZone::CheckoutHostTable,
                Direction::Down
            ),
            EscapeOutcome::Stop,
        );
        assert_eq!(
            escape_to_parent(
                TabId::Checkout,
                FocusZone::CheckoutHostTable,
                Direction::Left
            ),
            EscapeOutcome::Stop,
        );
        assert_eq!(
            escape_to_parent(
                TabId::Checkout,
                FocusZone::CheckoutControls,
                Direction::Down
            ),
            EscapeOutcome::SwitchZone(FocusZone::CheckoutHostTable),
        );
        assert_eq!(
            escape_to_parent(TabId::Checkout, FocusZone::CheckoutControls, Direction::Up),
            EscapeOutcome::Stop,
        );
    }

    #[test]
    fn breadcrumb_updates_on_zone_change() {
        let mut fp = FocusPath::for_tab(TabId::Checkout);
        let initial = fp.breadcrumb.clone();
        fp.switch_zone(TabId::Checkout, FocusZone::CheckoutControls);
        assert_ne!(initial, fp.breadcrumb);
        assert_eq!(fp.breadcrumb.last().unwrap(), "Controls");
    }
}
