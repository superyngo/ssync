//! Cursor + scroll-offset decoupling (per docs/tui_reconstruct_plan.md §11, AD-20).
//!
//! Invariant: `scroll_y <= selected <= scroll_y + visible_height - 1`.

#[derive(Debug, Clone, Default)]
pub struct Viewport {
    pub selected: usize,
    pub scroll_y: usize,
    pub visible_height: usize,
    pub item_count: usize,
}

impl Viewport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the runtime parameters for this frame.
    /// Re-clamps `selected` and `scroll_y` to maintain the invariant.
    pub fn set_dims(&mut self, item_count: usize, visible_height: usize) {
        self.item_count = item_count;
        self.visible_height = visible_height;
        self.clamp();
    }

    fn clamp(&mut self) {
        if self.item_count == 0 {
            self.selected = 0;
            self.scroll_y = 0;
            return;
        }
        if self.selected >= self.item_count {
            self.selected = self.item_count - 1;
        }
        if self.visible_height == 0 {
            self.scroll_y = self.selected;
            return;
        }
        let max_scroll = self.item_count.saturating_sub(self.visible_height);
        if self.scroll_y > max_scroll {
            self.scroll_y = max_scroll;
        }
        if self.selected < self.scroll_y {
            self.scroll_y = self.selected;
        } else if self.selected >= self.scroll_y + self.visible_height {
            self.scroll_y = self.selected + 1 - self.visible_height;
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.clamp();
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.item_count {
            self.selected += 1;
            self.clamp();
        }
    }

    pub fn page_up(&mut self) {
        let step = self.visible_height.max(1);
        self.selected = self.selected.saturating_sub(step);
        self.scroll_y = self.scroll_y.saturating_sub(step);
        self.clamp();
    }

    pub fn page_down(&mut self) {
        let step = self.visible_height.max(1);
        self.selected = (self.selected + step).min(self.item_count.saturating_sub(1));
        self.scroll_y = self.scroll_y.saturating_add(step);
        self.clamp();
    }

    pub fn home(&mut self) {
        self.selected = 0;
        self.scroll_y = 0;
    }

    pub fn end(&mut self) {
        if self.item_count == 0 {
            return;
        }
        self.selected = self.item_count - 1;
        self.clamp();
    }

    /// Range of indices currently visible: `[start, end)`.
    /// Rendering code should iterate this slice for O(visible) cost.
    pub fn visible_range(&self) -> (usize, usize) {
        let end = (self.scroll_y + self.visible_height).min(self.item_count);
        (self.scroll_y, end)
    }

    pub fn at_top(&self) -> bool {
        self.selected == 0
    }

    pub fn at_bottom(&self) -> bool {
        self.item_count == 0 || self.selected + 1 == self.item_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_holds_after_moves() {
        let mut v = Viewport::new();
        v.set_dims(100, 10);
        for _ in 0..50 {
            v.move_down();
        }
        assert!(v.scroll_y <= v.selected);
        assert!(v.selected < v.scroll_y + v.visible_height);
        assert_eq!(v.selected, 50);
    }

    #[test]
    fn page_down_advances_selection() {
        let mut v = Viewport::new();
        v.set_dims(100, 10);
        v.page_down();
        assert_eq!(v.selected, 10);
        v.end();
        assert_eq!(v.selected, 99);
    }

    #[test]
    fn empty_list_is_safe() {
        let mut v = Viewport::new();
        v.set_dims(0, 10);
        v.move_down();
        v.move_up();
        v.page_down();
        v.end();
        assert_eq!(v.visible_range(), (0, 0));
    }
}
