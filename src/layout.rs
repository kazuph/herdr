//! BSP tree layout for tiling panes within a workspace.

use ratatui::layout::{Direction, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PaneId(u32);

/// Global atomic counter for unique PaneId generation across all workspaces.
static NEXT_PANE_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl PaneId {
    /// Allocate a globally unique PaneId.
    pub fn alloc() -> Self {
        Self(NEXT_PANE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    pub fn raw(self) -> u32 {
        self.0
    }

    /// Reconstruct from a saved u32 (persistence only).
    pub fn from_raw(id: u32) -> Self {
        Self(id)
    }

    /// Keep future allocations above ids restored from a session snapshot.
    pub fn reserve_next_after(id: Self) {
        let next = id.0.saturating_add(1);
        let _ = NEXT_PANE_ID.fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |current| (current < next).then_some(next),
        );
    }
}

/// Snapshot of a pane's position and focus state after layout.
#[derive(Clone)]
pub struct PaneInfo {
    pub id: PaneId,
    /// Outer rect (including borders if present).
    pub rect: Rect,
    /// Inner rect (content area, excluding borders). Used for selection.
    pub inner_rect: Rect,
    /// Visible scrollbar lane, when scrollback is present. `inner_rect` may still
    /// exclude a stable hidden gutter when this is `None`.
    pub scrollbar_rect: Option<Rect>,
    pub is_focused: bool,
}

/// Info about a split boundary, used for mouse drag resize.
#[derive(Clone)]
pub struct SplitBorder {
    /// Position of the divider line (x for horizontal split, y for vertical).
    pub pos: u16,
    /// Direction of the split that created this border.
    pub direction: Direction,
    /// Total area of the split node.
    pub area: Rect,
    /// Path from root to this split node (false=first, true=second).
    pub path: Vec<bool>,
}

/// Cardinal direction for pane navigation.
#[derive(Debug, Clone, Copy)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSplitSide {
    First,
    Second,
}

/// A node in the BSP tree. Public for serialization.
#[derive(Clone)]
pub enum Node {
    Pane(PaneId),
    Split {
        direction: Direction,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// BSP tiling layout. Tracks a tree of splits and a focused pane.
#[derive(Clone)]
pub struct TileLayout {
    root: Node,
    focus: PaneId,
    order: Vec<PaneId>,
}

impl TileLayout {
    /// Create a new layout with a single pane (globally unique ID).
    /// Returns (layout, root_pane_id) so the caller can create the pane.
    pub fn new() -> (Self, PaneId) {
        let root_id = PaneId::alloc();
        (Self::new_with_pane(root_id), root_id)
    }

    pub fn new_with_pane(root_id: PaneId) -> Self {
        PaneId::reserve_next_after(root_id);
        Self {
            root: Node::Pane(root_id),
            focus: root_id,
            order: vec![root_id],
        }
    }

    pub fn focused(&self) -> PaneId {
        self.focus
    }

    pub fn pane_count(&self) -> usize {
        count_panes(&self.root)
    }

    /// Compute rects for all panes given the available area.
    pub fn panes(&self, area: Rect) -> Vec<PaneInfo> {
        let mut result = Vec::new();
        collect_panes(&self.root, area, self.focus, &mut result);
        result
    }

    /// Collect all split boundaries for mouse drag resize.
    pub fn splits(&self, area: Rect) -> Vec<SplitBorder> {
        let mut result = Vec::new();
        collect_splits(&self.root, area, vec![], &mut result);
        result
    }

    /// Split the focused pane. Returns the new pane's id.
    pub fn split_focused(&mut self, direction: Direction) -> PaneId {
        let new_id = PaneId::alloc();
        self.split_focused_with_pane(direction, new_id);
        new_id
    }

    pub fn split_focused_with_pane(&mut self, direction: Direction, new_id: PaneId) {
        PaneId::reserve_next_after(new_id);
        let placeholder = PaneId::from_raw(0);
        let old = std::mem::replace(&mut self.root, Node::Pane(placeholder));
        self.root = split_at(old, self.focus, direction, new_id);
        let insert_at = self
            .order
            .iter()
            .position(|id| *id == self.focus)
            .map_or(self.order.len(), |index| index + 1);
        self.order.insert(insert_at, new_id);
        self.focus = new_id;
    }

    /// Close the focused pane. Returns false if it's the last pane.
    pub fn close_focused(&mut self) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }
        let target = self.focus;
        let ids = self.pane_ids();
        let pos = ids.iter().position(|id| *id == target).unwrap();
        let new_focus = if pos + 1 < ids.len() {
            ids[pos + 1]
        } else {
            ids[pos - 1]
        };
        let placeholder = PaneId::from_raw(0);
        let old = std::mem::replace(&mut self.root, Node::Pane(placeholder));
        if let Some(new_root) = remove_pane(old, target) {
            self.root = new_root;
            self.order.retain(|id| *id != target);
            self.focus = new_focus;
            true
        } else {
            false
        }
    }

    pub fn focus_next(&mut self) {
        let ids = self.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == self.focus) {
            self.focus = ids[(pos + 1) % ids.len()];
        }
    }

    pub fn focus_prev(&mut self) {
        let ids = self.pane_ids();
        if let Some(pos) = ids.iter().position(|id| *id == self.focus) {
            self.focus = ids[(pos + ids.len() - 1) % ids.len()];
        }
    }

    pub fn focus_pane(&mut self, id: PaneId) {
        if self.pane_ids().contains(&id) {
            self.focus = id;
        }
    }

    /// Set the ratio of a split node at the given path.
    pub fn set_ratio_at(&mut self, path: &[bool], ratio: f32) {
        set_ratio_at(&mut self.root, path, ratio.clamp(0.1, 0.9));
    }

    /// Rebuild every pane in one split direction while preserving pane order.
    #[cfg(test)]
    pub fn arrange_all(&mut self, direction: Direction) {
        let ids = self.pane_ids();
        if ids.len() <= 1 {
            return;
        }
        self.root = build_even_split(&ids, direction);
        if !ids.contains(&self.focus) {
            self.focus = ids[0];
        }
    }

    /// Move the focused pane to a specific side of a root split.
    pub fn move_focused_to_root_split_side(
        &mut self,
        direction: Direction,
        side: RootSplitSide,
    ) -> bool {
        if self.pane_count() <= 1 {
            return false;
        }

        let target = self.focus;
        let placeholder = PaneId::from_raw(0);
        let old = std::mem::replace(&mut self.root, Node::Pane(placeholder));
        let Some(remaining) = remove_pane(old, target) else {
            self.root = Node::Pane(target);
            return false;
        };
        let total = count_panes(&remaining) + 1;
        let target_ratio = 1.0 / total as f32;
        let remaining_ratio = (total - 1) as f32 / total as f32;
        let (ratio, first, second) = match side {
            RootSplitSide::First => (
                target_ratio,
                Box::new(Node::Pane(target)),
                Box::new(remaining),
            ),
            RootSplitSide::Second => (
                remaining_ratio,
                Box::new(remaining),
                Box::new(Node::Pane(target)),
            ),
        };
        self.root = Node::Split {
            direction,
            ratio,
            first,
            second,
        };
        self.order = collect_node_ids(&self.root);
        self.focus = target;
        true
    }

    /// Cycle through broad pane layout presets while preserving user order.
    pub fn cycle_layout(&mut self) -> bool {
        let ids = self.pane_ids();
        if ids.len() <= 1 {
            return false;
        }

        if ids.len() == 2 {
            let direction = if is_uniform_split(&self.root, Direction::Horizontal) {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            self.root = build_even_split(&ids, direction);
            return true;
        }

        if is_uniform_split(&self.root, Direction::Horizontal) {
            self.root = build_even_split(&ids, Direction::Vertical);
        } else if is_uniform_split(&self.root, Direction::Vertical) {
            self.root = build_two_row_grid(&ids);
        } else if is_balanced_two_row_grid(&self.root, &ids) {
            self.root = build_main_split(&ids, Direction::Horizontal, build_two_row_grid);
        } else if is_main_first_split(&self.root, Direction::Horizontal, is_two_row_grid, ids[0]) {
            self.root = build_main_split_reversed(&ids, Direction::Horizontal, build_two_row_grid);
        } else if is_main_second_split(&self.root, Direction::Horizontal, is_two_row_grid, ids[0]) {
            self.root = build_main_split(&ids, Direction::Vertical, |ids| {
                build_even_split(ids, Direction::Horizontal)
            });
        } else if is_main_first_split(
            &self.root,
            Direction::Vertical,
            |node| is_uniform_split(node, Direction::Horizontal),
            ids[0],
        ) {
            self.root = build_main_split_reversed(&ids, Direction::Vertical, |ids| {
                build_even_split(ids, Direction::Horizontal)
            });
        } else {
            self.root = build_even_split(&ids, Direction::Horizontal);
        }
        if !ids.contains(&self.focus) {
            self.focus = ids[0];
        }
        true
    }

    /// Rotate pane identities through the existing leaf positions.
    ///
    /// This preserves the split tree shape and keeps each pane's terminal state
    /// attached to its PaneId, so external commands targeting `%N` continue to
    /// follow the same pane after rotation.
    pub fn rotate_panes(&mut self, reverse: bool) -> bool {
        let mut ids = self.pane_ids();
        if ids.len() <= 1 {
            return false;
        }
        if reverse {
            ids.rotate_left(1);
        } else {
            ids.rotate_right(1);
        }
        self.order.clone_from(&ids);
        let mut rotated = ids.into_iter();
        replace_leaf_ids(&mut self.root, &mut rotated);
        true
    }

    /// Equalize split ratios while preserving the current split directions.
    pub fn equalize(&mut self) {
        equalize_ratios(&mut self.root);
    }

    /// Adjust the nearest split in the given direction for the focused pane.
    /// `delta` is positive to grow, negative to shrink.
    pub fn resize_focused(&mut self, nav: NavDirection, delta: f32, area: Rect) {
        let panes = self.panes(area);
        let Some(focused) = panes.iter().find(|p| p.is_focused) else {
            return;
        };
        let focused_rect = focused.rect;
        let splits = self.splits(area);

        // Find the split whose border is adjacent to the focused pane in the given direction
        let target_dir = match nav {
            NavDirection::Left | NavDirection::Right => Direction::Horizontal,
            NavDirection::Up | NavDirection::Down => Direction::Vertical,
        };
        let grows = matches!(nav, NavDirection::Right | NavDirection::Down);

        // Find the closest matching split border
        let best = splits
            .iter()
            .filter(|s| s.direction == target_dir)
            .filter(|s| match target_dir {
                Direction::Horizontal => {
                    // Border must be near the focused pane's left or right edge
                    let near_right = (s.pos as i32 - (focused_rect.x + focused_rect.width) as i32)
                        .unsigned_abs()
                        <= 1;
                    let near_left = (s.pos as i32 - focused_rect.x as i32).unsigned_abs() <= 1;
                    near_right || near_left
                }
                Direction::Vertical => {
                    let near_bottom = (s.pos as i32
                        - (focused_rect.y + focused_rect.height) as i32)
                        .unsigned_abs()
                        <= 1;
                    let near_top = (s.pos as i32 - focused_rect.y as i32).unsigned_abs() <= 1;
                    near_bottom || near_top
                }
            })
            .min_by_key(|s| {
                // Prefer the border in the direction we're resizing toward
                match (target_dir, grows) {
                    (Direction::Horizontal, true) => {
                        ((focused_rect.x + focused_rect.width) as i32 - s.pos as i32).unsigned_abs()
                    }
                    (Direction::Horizontal, false) => {
                        (focused_rect.x as i32 - s.pos as i32).unsigned_abs()
                    }
                    (Direction::Vertical, true) => ((focused_rect.y + focused_rect.height) as i32
                        - s.pos as i32)
                        .unsigned_abs(),
                    (Direction::Vertical, false) => {
                        (focused_rect.y as i32 - s.pos as i32).unsigned_abs()
                    }
                }
            });

        if let Some(split) = best {
            let path = split.path.clone();
            let current_ratio = get_ratio_at(&self.root, &path).unwrap_or(0.5);
            let adj = if grows { delta } else { -delta };
            self.set_ratio_at(&path, current_ratio + adj);
        }
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.order.clone()
    }

    /// Access the tree root for serialization.
    pub fn root(&self) -> &Node {
        &self.root
    }

    /// Reconstruct a layout from a saved tree.
    /// Reconstruct a layout from a saved tree.
    pub fn from_saved(root: Node, focus: PaneId, saved_order: &[PaneId]) -> Self {
        let tree_order = collect_node_ids(&root);
        let mut order = Vec::with_capacity(tree_order.len());
        for id in saved_order.iter().chain(&tree_order) {
            if tree_order.contains(id) && !order.contains(id) {
                order.push(*id);
            }
        }
        Self { root, focus, order }
    }
}

// --- Directional pane navigation ---

/// Find the nearest pane in the given direction from `focused`.
pub fn find_in_direction(
    focused: &PaneInfo,
    direction: NavDirection,
    panes: &[PaneInfo],
) -> Option<PaneId> {
    let fr = focused.rect;

    panes
        .iter()
        .filter(|p| p.id != focused.id)
        .filter(|p| {
            let r = p.rect;
            match direction {
                NavDirection::Left => {
                    r.x + r.width <= fr.x && ranges_overlap(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Right => {
                    r.x >= fr.x + fr.width && ranges_overlap(r.y, r.height, fr.y, fr.height)
                }
                NavDirection::Up => {
                    r.y + r.height <= fr.y && ranges_overlap(r.x, r.width, fr.x, fr.width)
                }
                NavDirection::Down => {
                    r.y >= fr.y + fr.height && ranges_overlap(r.x, r.width, fr.x, fr.width)
                }
            }
        })
        .min_by_key(|p| {
            let r = p.rect;
            match direction {
                NavDirection::Left => fr.x.saturating_sub(r.x + r.width),
                NavDirection::Right => r.x.saturating_sub(fr.x + fr.width),
                NavDirection::Up => fr.y.saturating_sub(r.y + r.height),
                NavDirection::Down => r.y.saturating_sub(fr.y + fr.height),
            }
        })
        .map(|p| p.id)
}

fn ranges_overlap(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> bool {
    a_start < b_start + b_len && a_start + a_len > b_start
}

// --- Tree operations ---

fn count_panes(node: &Node) -> usize {
    match node {
        Node::Pane(_) => 1,
        Node::Split { first, second, .. } => count_panes(first) + count_panes(second),
    }
}

fn collect_panes(node: &Node, area: Rect, focus: PaneId, result: &mut Vec<PaneInfo>) {
    match node {
        Node::Pane(id) => {
            result.push(PaneInfo {
                id: *id,
                rect: area,
                // inner_rect is set during render when we know if borders are shown
                inner_rect: area,
                scrollbar_rect: None,
                is_focused: *id == focus,
            });
        }
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (a, b) = split_rect(area, *direction, *ratio);
            collect_panes(first, a, focus, result);
            collect_panes(second, b, focus, result);
        }
    }
}

fn collect_splits(node: &Node, area: Rect, path: Vec<bool>, result: &mut Vec<SplitBorder>) {
    if let Node::Split {
        direction,
        ratio,
        first,
        second,
    } = node
    {
        let (a, b) = split_rect(area, *direction, *ratio);
        let pos = match direction {
            Direction::Horizontal => a.x + a.width,
            Direction::Vertical => a.y + a.height,
        };
        result.push(SplitBorder {
            pos,
            direction: *direction,
            area,
            path: path.clone(),
        });
        let mut lp = path.clone();
        lp.push(false);
        collect_splits(first, a, lp, result);
        let mut rp = path;
        rp.push(true);
        collect_splits(second, b, rp, result);
    }
}

fn collect_ids(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids(first, ids);
            collect_ids(second, ids);
        }
    }
}

fn collect_node_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids(node, &mut ids);
    ids
}

fn replace_leaf_ids(node: &mut Node, ids: &mut impl Iterator<Item = PaneId>) {
    match node {
        Node::Pane(id) => {
            if let Some(next) = ids.next() {
                *id = next;
            }
        }
        Node::Split { first, second, .. } => {
            replace_leaf_ids(first, ids);
            replace_leaf_ids(second, ids);
        }
    }
}

fn build_even_split(ids: &[PaneId], direction: Direction) -> Node {
    match ids {
        [] => Node::Pane(PaneId::from_raw(0)),
        [id] => Node::Pane(*id),
        [first, rest @ ..] => Node::Split {
            direction,
            ratio: 1.0 / ids.len() as f32,
            first: Box::new(Node::Pane(*first)),
            second: Box::new(build_even_split(rest, direction)),
        },
    }
}

fn build_main_split(
    ids: &[PaneId],
    root_direction: Direction,
    build_rest: impl Fn(&[PaneId]) -> Node,
) -> Node {
    match ids {
        [] => Node::Pane(PaneId::from_raw(0)),
        [id] => Node::Pane(*id),
        [first, rest @ ..] => Node::Split {
            direction: root_direction,
            ratio: 0.5,
            first: Box::new(Node::Pane(*first)),
            second: Box::new(build_rest(rest)),
        },
    }
}

fn build_main_split_reversed(
    ids: &[PaneId],
    root_direction: Direction,
    build_rest: impl Fn(&[PaneId]) -> Node,
) -> Node {
    match ids {
        [] => Node::Pane(PaneId::from_raw(0)),
        [id] => Node::Pane(*id),
        [first, rest @ ..] => Node::Split {
            direction: root_direction,
            ratio: 0.5,
            first: Box::new(build_rest(rest)),
            second: Box::new(Node::Pane(*first)),
        },
    }
}

fn build_two_row_grid(ids: &[PaneId]) -> Node {
    match ids {
        [] => Node::Pane(PaneId::from_raw(0)),
        [id] => Node::Pane(*id),
        _ => {
            let top_count = ids.len().div_ceil(2);
            let (top, bottom) = ids.split_at(top_count);
            Node::Split {
                direction: Direction::Vertical,
                ratio: 0.5,
                first: Box::new(build_even_split(top, Direction::Horizontal)),
                second: Box::new(build_even_split(bottom, Direction::Horizontal)),
            }
        }
    }
}

fn is_uniform_split(node: &Node, expected: Direction) -> bool {
    match node {
        Node::Pane(_) => true,
        Node::Split {
            direction,
            first,
            second,
            ..
        } => {
            *direction == expected
                && is_uniform_split(first, expected)
                && is_uniform_split(second, expected)
        }
    }
}

fn is_two_row_grid(node: &Node) -> bool {
    match node {
        Node::Pane(_) => true,
        Node::Split {
            direction,
            first,
            second,
            ..
        } if *direction == Direction::Vertical => {
            is_uniform_split(first, Direction::Horizontal)
                && is_uniform_split(second, Direction::Horizontal)
        }
        _ => false,
    }
}

fn is_balanced_two_row_grid(node: &Node, order: &[PaneId]) -> bool {
    let Node::Split {
        direction,
        first,
        second,
        ..
    } = node
    else {
        return false;
    };
    *direction == Direction::Vertical
        && is_uniform_split(first, Direction::Horizontal)
        && is_uniform_split(second, Direction::Horizontal)
        && count_panes(first) == order.len().div_ceil(2)
        && count_panes(second) == order.len() / 2
        && collect_node_ids(node) == order
}

fn is_main_first_split(
    node: &Node,
    root_direction: Direction,
    rest_matches: impl Fn(&Node) -> bool,
    first_pane: PaneId,
) -> bool {
    match node {
        Node::Split {
            direction,
            first,
            second,
            ..
        } if *direction == root_direction => {
            matches!(first.as_ref(), Node::Pane(id) if *id == first_pane) && rest_matches(second)
        }
        _ => false,
    }
}

fn is_main_second_split(
    node: &Node,
    root_direction: Direction,
    rest_matches: impl Fn(&Node) -> bool,
    first_pane: PaneId,
) -> bool {
    match node {
        Node::Split {
            direction,
            first,
            second,
            ..
        } if *direction == root_direction => {
            rest_matches(first) && matches!(second.as_ref(), Node::Pane(id) if *id == first_pane)
        }
        _ => false,
    }
}

fn split_at(node: Node, target: PaneId, direction: Direction, new_id: PaneId) -> Node {
    match node {
        Node::Pane(id) if id == target => Node::Split {
            direction,
            ratio: 0.5,
            first: Box::new(Node::Pane(id)),
            second: Box::new(Node::Pane(new_id)),
        },
        Node::Pane(_) => node,
        Node::Split {
            direction: d,
            ratio,
            first,
            second,
        } => Node::Split {
            direction: d,
            ratio,
            first: Box::new(split_at(*first, target, direction, new_id)),
            second: Box::new(split_at(*second, target, direction, new_id)),
        },
    }
}

fn remove_pane(node: Node, target: PaneId) -> Option<Node> {
    match node {
        Node::Pane(id) if id == target => None,
        Node::Pane(_) => Some(node),
        Node::Split {
            direction,
            ratio,
            first,
            second,
        } => match (remove_pane(*first, target), remove_pane(*second, target)) {
            (None, Some(s)) => Some(s),
            (Some(f), None) => Some(f),
            (Some(f), Some(s)) => Some(Node::Split {
                direction,
                ratio,
                first: Box::new(f),
                second: Box::new(s),
            }),
            (None, None) => None,
        },
    }
}

fn equalize_ratios(node: &mut Node) -> usize {
    match node {
        Node::Pane(_) => 1,
        Node::Split {
            ratio,
            first,
            second,
            ..
        } => {
            let first_count = equalize_ratios(first);
            let second_count = equalize_ratios(second);
            let total = first_count + second_count;
            *ratio = first_count as f32 / total as f32;
            total
        }
    }
}

fn set_ratio_at(node: &mut Node, path: &[bool], new_ratio: f32) {
    if let Node::Split {
        ratio,
        first,
        second,
        ..
    } = node
    {
        if path.is_empty() {
            *ratio = new_ratio;
        } else if path[0] {
            set_ratio_at(second, &path[1..], new_ratio);
        } else {
            set_ratio_at(first, &path[1..], new_ratio);
        }
    }
}

fn get_ratio_at(node: &Node, path: &[bool]) -> Option<f32> {
    if let Node::Split {
        ratio,
        first,
        second,
        ..
    } = node
    {
        if path.is_empty() {
            Some(*ratio)
        } else if path[0] {
            get_ratio_at(second, &path[1..])
        } else {
            get_ratio_at(first, &path[1..])
        }
    } else {
        None
    }
}

fn split_rect(area: Rect, direction: Direction, ratio: f32) -> (Rect, Rect) {
    match direction {
        Direction::Horizontal => {
            let first_w = ((area.width as f32) * ratio).round() as u16;
            let second_w = area.width.saturating_sub(first_w);
            (
                Rect::new(area.x, area.y, first_w, area.height),
                Rect::new(area.x + first_w, area.y, second_w, area.height),
            )
        }
        Direction::Vertical => {
            let first_h = ((area.height as f32) * ratio).round() as u16;
            let second_h = area.height.saturating_sub(first_h);
            (
                Rect::new(area.x, area.y, area.width, first_h),
                Rect::new(area.x, area.y + first_h, area.width, second_h),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrange_all_preserves_order_and_stacks_multiple_panes() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(second);

        layout.arrange_all(Direction::Vertical);

        assert_eq!(layout.pane_ids(), vec![root, second, third]);
        assert_eq!(layout.focused(), second);
        let panes = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 90, 10));
        assert_eq!(panes[1].rect, Rect::new(0, 10, 90, 10));
        assert_eq!(panes[2].rect, Rect::new(0, 20, 90, 10));
    }

    #[test]
    fn arrange_all_lays_multiple_panes_side_by_side() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Vertical);
        let third = layout.split_focused(Direction::Vertical);

        layout.arrange_all(Direction::Horizontal);

        assert_eq!(layout.pane_ids(), vec![root, second, third]);
        let panes = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 30, 30));
        assert_eq!(panes[1].rect, Rect::new(30, 0, 30, 30));
        assert_eq!(panes[2].rect, Rect::new(60, 0, 30, 30));
    }

    #[test]
    fn move_focused_to_root_split_second_only_moves_target_pane() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(second);

        assert!(layout.move_focused_to_root_split_side(Direction::Vertical, RootSplitSide::Second));

        assert_eq!(layout.pane_ids(), vec![root, third, second]);
        assert_eq!(layout.focused(), second);
        let panes = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 45, 20));
        assert_eq!(panes[1].rect, Rect::new(45, 0, 45, 20));
        assert_eq!(panes[2].rect, Rect::new(0, 20, 90, 10));
        for _ in 0..7 {
            assert!(layout.cycle_layout());
            assert_eq!(layout.pane_ids(), vec![root, third, second]);
        }
    }

    #[test]
    fn move_focused_to_root_split_side_can_place_target_first() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(second);

        assert!(layout.move_focused_to_root_split_side(Direction::Horizontal, RootSplitSide::First));

        assert_eq!(layout.pane_ids(), vec![second, root, third]);
        assert_eq!(layout.focused(), second);
        let panes = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 30, 30));
        assert_eq!(panes[1].rect, Rect::new(30, 0, 30, 30));
        assert_eq!(panes[2].rect, Rect::new(60, 0, 30, 30));
        for _ in 0..7 {
            assert!(layout.cycle_layout());
            assert_eq!(layout.pane_ids(), vec![second, root, third]);
        }
    }

    #[test]
    fn move_focused_to_root_split_side_can_place_target_rightmost() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(second);

        assert!(
            layout.move_focused_to_root_split_side(Direction::Horizontal, RootSplitSide::Second,)
        );

        assert_eq!(layout.pane_ids(), vec![root, third, second]);
        for _ in 0..7 {
            assert!(layout.cycle_layout());
            assert_eq!(layout.pane_ids(), vec![root, third, second]);
        }
    }

    #[test]
    fn move_focused_to_root_split_side_can_place_target_upper() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(second);

        assert!(layout.move_focused_to_root_split_side(Direction::Vertical, RootSplitSide::First));

        assert_eq!(layout.pane_ids(), vec![second, root, third]);
        assert_eq!(layout.focused(), second);
        let panes = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 90, 10));
        assert_eq!(panes[1].rect, Rect::new(0, 10, 45, 20));
        assert_eq!(panes[2].rect, Rect::new(45, 10, 45, 20));
        for _ in 0..7 {
            assert!(layout.cycle_layout());
            assert_eq!(layout.pane_ids(), vec![second, root, third]);
        }
    }

    #[test]
    fn cycle_layout_steps_through_layout_presets() {
        let (mut layout, root) = TileLayout::new();
        let mut ids = vec![root];
        for _ in 0..8 {
            ids.push(layout.split_focused(Direction::Horizontal));
        }
        layout.arrange_all(Direction::Horizontal);
        layout.focus_pane(ids[1]);
        let rect_of = |panes: &[PaneInfo], id: PaneId| -> Rect {
            panes.iter().find(|pane| pane.id == id).unwrap().rect
        };

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        assert_eq!(layout.focused(), ids[1]);
        let panes = layout.panes(Rect::new(0, 0, 180, 45));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(0, 0, 180, 5));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(0, 40, 180, 5));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        let panes = layout.panes(Rect::new(0, 0, 180, 40));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(0, 0, 36, 20));
        assert_eq!(rect_of(&panes, ids[4]), Rect::new(144, 0, 36, 20));
        assert_eq!(rect_of(&panes, ids[5]), Rect::new(0, 20, 45, 20));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(135, 20, 45, 20));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        let panes = layout.panes(Rect::new(0, 0, 160, 40));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(0, 0, 80, 40));
        assert_eq!(rect_of(&panes, ids[1]), Rect::new(80, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[2]), Rect::new(100, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[3]), Rect::new(120, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[4]), Rect::new(140, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[5]), Rect::new(80, 20, 20, 20));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(140, 20, 20, 20));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        let panes = layout.panes(Rect::new(0, 0, 160, 40));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(80, 0, 80, 40));
        assert_eq!(rect_of(&panes, ids[1]), Rect::new(0, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[4]), Rect::new(60, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[5]), Rect::new(0, 20, 20, 20));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(60, 20, 20, 20));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        let panes = layout.panes(Rect::new(0, 0, 160, 40));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(0, 0, 160, 20));
        assert_eq!(rect_of(&panes, ids[1]), Rect::new(0, 20, 20, 20));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(140, 20, 20, 20));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        let panes = layout.panes(Rect::new(0, 0, 160, 40));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(0, 20, 160, 20));
        assert_eq!(rect_of(&panes, ids[1]), Rect::new(0, 0, 20, 20));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(140, 0, 20, 20));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        let panes = layout.panes(Rect::new(0, 0, 180, 40));
        assert_eq!(rect_of(&panes, ids[0]), Rect::new(0, 0, 20, 40));
        assert_eq!(rect_of(&panes, ids[8]), Rect::new(160, 0, 20, 40));
    }

    #[test]
    fn cycle_layout_toggles_two_panes_between_vertical_and_horizontal() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Vertical);
        layout.focus_pane(root);
        let ids = vec![root, second];

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        assert_eq!(layout.focused(), root);
        let panes = layout.panes(Rect::new(0, 0, 120, 40));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 60, 40));
        assert_eq!(panes[1].rect, Rect::new(60, 0, 60, 40));

        assert!(layout.cycle_layout());
        assert_eq!(layout.pane_ids(), ids);
        assert_eq!(layout.focused(), root);
        let panes = layout.panes(Rect::new(0, 0, 120, 40));
        assert_eq!(panes[0].rect, Rect::new(0, 0, 120, 20));
        assert_eq!(panes[1].rect, Rect::new(0, 20, 120, 20));
    }

    #[test]
    fn equal_grid_balances_even_and_odd_pane_counts() {
        for pane_count in [4_usize, 5] {
            let (mut layout, _) = TileLayout::new();
            for _ in 1..pane_count {
                layout.split_focused(Direction::Horizontal);
            }
            layout.arrange_all(Direction::Horizontal);

            assert!(layout.cycle_layout());
            assert!(layout.cycle_layout());

            let panes = layout.panes(Rect::new(0, 0, 120, 40));
            let top = panes.iter().filter(|pane| pane.rect.y == 0).count();
            let bottom = panes.iter().filter(|pane| pane.rect.y == 20).count();
            assert_eq!(top, pane_count.div_ceil(2));
            assert_eq!(bottom, pane_count / 2);
            assert!(panes.iter().all(|pane| pane.rect.height == 20));
            assert_eq!(
                panes
                    .iter()
                    .map(|pane| u32::from(pane.rect.width) * u32::from(pane.rect.height))
                    .sum::<u32>(),
                120 * 40
            );
            for (index, pane) in panes.iter().enumerate() {
                for other in panes.iter().skip(index + 1) {
                    let overlaps = pane.rect.x < other.rect.x + other.rect.width
                        && pane.rect.x + pane.rect.width > other.rect.x
                        && pane.rect.y < other.rect.y + other.rect.height
                        && pane.rect.y + pane.rect.height > other.rect.y;
                    assert!(!overlaps, "grid panes must not overlap");
                }
            }
        }
    }

    #[test]
    fn reordered_panes_stay_ordered_across_cycle_and_rotation() {
        let (mut layout, first) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Horizontal);
        let rightmost = layout.split_focused(Direction::Horizontal);
        layout.focus_pane(rightmost);
        assert!(
            layout.move_focused_to_root_split_side(Direction::Horizontal, RootSplitSide::First,)
        );
        let reordered = vec![rightmost, first, second, third];
        assert_eq!(layout.pane_ids(), reordered);

        for _ in 0..7 {
            assert!(layout.cycle_layout());
            assert_eq!(layout.pane_ids(), reordered);
        }

        assert!(layout.rotate_panes(false));
        assert_eq!(layout.pane_ids(), vec![third, rightmost, first, second]);
        assert!(layout.rotate_panes(true));
        assert_eq!(layout.pane_ids(), reordered);
    }

    #[test]
    fn saved_logical_order_survives_layout_restore() {
        let first = PaneId::from_raw(41);
        let second = PaneId::from_raw(42);
        let third = PaneId::from_raw(43);
        let root = build_even_split(&[first, second, third], Direction::Horizontal);

        let layout = TileLayout::from_saved(root, third, &[third, first, second]);

        assert_eq!(layout.pane_ids(), vec![third, first, second]);
        assert_eq!(layout.focused(), third);
    }

    #[test]
    fn closing_grid_leaf_reflows_survivors_over_the_full_viewport() {
        let (mut layout, _) = TileLayout::new();
        for _ in 1..5 {
            layout.split_focused(Direction::Horizontal);
        }
        layout.arrange_all(Direction::Horizontal);
        assert!(layout.cycle_layout());
        assert!(layout.cycle_layout());

        assert!(layout.close_focused());

        let viewport = Rect::new(0, 0, 120, 40);
        let panes = layout.panes(viewport);
        assert_eq!(panes.len(), 4);
        assert_eq!(
            panes
                .iter()
                .map(|pane| u32::from(pane.rect.width) * u32::from(pane.rect.height))
                .sum::<u32>(),
            u32::from(viewport.width) * u32::from(viewport.height)
        );
    }

    #[test]
    fn rotate_panes_rotates_ids_through_existing_leaf_positions() {
        let (mut layout, root) = TileLayout::new();
        let second = layout.split_focused(Direction::Horizontal);
        let third = layout.split_focused(Direction::Vertical);
        let before = layout.panes(Rect::new(0, 0, 90, 30));
        let rect_of = |panes: &[PaneInfo], id: PaneId| -> Rect {
            panes.iter().find(|pane| pane.id == id).unwrap().rect
        };
        let root_rect = rect_of(&before, root);
        let second_rect = rect_of(&before, second);
        let third_rect = rect_of(&before, third);

        assert!(layout.rotate_panes(false));

        assert_eq!(layout.pane_ids(), vec![third, root, second]);
        let after = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(rect_of(&after, third), root_rect);
        assert_eq!(rect_of(&after, root), second_rect);
        assert_eq!(rect_of(&after, second), third_rect);
    }

    #[test]
    fn equalize_preserves_directions_and_balances_leaf_sizes() {
        let (mut layout, _root) = TileLayout::new();
        layout.split_focused(Direction::Horizontal);
        layout.split_focused(Direction::Horizontal);
        let order = layout.pane_ids();
        layout.set_ratio_at(&[], 0.8);
        layout.set_ratio_at(&[true], 0.8);

        layout.equalize();

        assert_eq!(layout.pane_ids(), order);
        let panes = layout.panes(Rect::new(0, 0, 90, 30));
        assert_eq!(panes[0].rect.width, 30);
        assert_eq!(panes[1].rect.width, 30);
        assert_eq!(panes[2].rect.width, 30);
    }
}
