//! The hit map (05 section 6.1): every widget registers a `HitRegion` while it draws, the map is
//! rebuilt each frame, and a pointer cell resolves to the topmost region there. It lives in the
//! TUI crate, not in the graph.

use aibus_core::Pane;
use ratatui::layout::Rect;

/// What a region is, which is also what a click on it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitId {
    Pane(Pane),
    /// The nth card of the cards view.
    Card(usize),
    /// The nth line of the log view.
    LogRow(usize),
    /// The nth advisor checkpoint diamond.
    Checkpoint(usize),
    /// The 3-column rail strip.
    RailStrip,
    /// The border between the rail and the centre column.
    RailHandle,
    /// The border above the session log.
    LogHandle,
    Track(Pane),
    Thumb(Pane),
    /// A full-screen region just under a modal.
    Backdrop,
    /// The modal's own box.
    Modal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    /// Focuses.
    Pane,
    /// Focuses and selects; a double click opens.
    Item,
    /// Dragged to resize.
    Handle,
    /// Clicked to page.
    Track,
    /// Dragged to scroll.
    Thumb,
    /// Swallows the click.
    Modal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitRegion {
    pub rect: Rect,
    pub id: HitId,
    pub kind: HitKind,
    /// Higher is on top. Equal `z`: the one registered later.
    pub z: u8,
}

/// Layers, so the widgets agree on who is above whom.
pub mod z {
    pub const PANE: u8 = 0;
    pub const ITEM: u8 = 1;
    pub const TRACK: u8 = 2;
    pub const THUMB: u8 = 3;
    pub const HANDLE: u8 = 4;
    pub const OVERLAY: u8 = 5;
    pub const BACKDROP: u8 = 10;
    pub const MODAL: u8 = 11;
}

#[derive(Clone, Debug, Default)]
pub struct HitMap {
    regions: Vec<HitRegion>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn add(&mut self, rect: Rect, id: HitId, kind: HitKind, z: u8) {
        if rect.width > 0 && rect.height > 0 {
            self.regions.push(HitRegion { rect, id, kind, z });
        }
    }

    /// The topmost region containing the cell.
    pub fn at(&self, col: u16, row: u16) -> Option<&HitRegion> {
        self.regions
            .iter()
            .filter(|r| r.rect.contains((col, row).into()))
            .max_by_key(|r| r.z)
            // `max_by_key` keeps the last of equals, which is the one registered later.
    }

    pub fn regions(&self) -> &[HitRegion] {
        &self.regions
    }

    /// The region with this id, if it was drawn.
    pub fn find(&self, id: HitId) -> Option<&HitRegion> {
        self.regions.iter().find(|r| r.id == id)
    }

    /// Whether anything on screen reacts to the pointer resting on it, which is what decides
    /// whether move events are worth asking the terminal for.
    pub fn hoverable(&self) -> bool {
        self.regions.iter().any(|r| !matches!(r.kind, HitKind::Modal))
            && !self.regions.iter().any(|r| r.id == HitId::Backdrop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> HitMap {
        let mut m = HitMap::default();
        // The dashboard: a log pane with a row, a thumb and a handle, then a rail overlay.
        m.add(Rect::new(0, 0, 80, 24), HitId::Pane(Pane::Input), HitKind::Pane, z::PANE);
        m.add(Rect::new(0, 16, 80, 6), HitId::Pane(Pane::Log), HitKind::Pane, z::PANE);
        m.add(Rect::new(1, 17, 70, 1), HitId::LogRow(0), HitKind::Item, z::ITEM);
        m.add(Rect::new(79, 17, 1, 4), HitId::Track(Pane::Log), HitKind::Track, z::TRACK);
        m.add(Rect::new(79, 18, 1, 2), HitId::Thumb(Pane::Log), HitKind::Thumb, z::THUMB);
        m.add(Rect::new(0, 16, 80, 1), HitId::LogHandle, HitKind::Handle, z::HANDLE);
        m.add(Rect::new(0, 3, 22, 12), HitId::Pane(Pane::Advisor), HitKind::Pane, z::OVERLAY);
        m.add(Rect::new(1, 5, 20, 1), HitId::Checkpoint(0), HitKind::Item, z::OVERLAY + 1);
        m
    }

    /// The same point on every layer: the topmost region wins.
    #[test]
    fn a_point_resolves_to_the_topmost_region() {
        let m = map();
        let table = [
            ((40, 2), Some(HitId::Pane(Pane::Input))),
            ((40, 18), Some(HitId::Pane(Pane::Log))),
            ((5, 17), Some(HitId::LogRow(0))),
            ((79, 17), Some(HitId::Track(Pane::Log))),
            ((79, 18), Some(HitId::Thumb(Pane::Log))),
            ((79, 20), Some(HitId::Track(Pane::Log))),
            // The handle is above the row that shares the border row.
            ((5, 16), Some(HitId::LogHandle)),
            ((79, 16), Some(HitId::LogHandle)),
            ((10, 8), Some(HitId::Pane(Pane::Advisor))),
            ((10, 5), Some(HitId::Checkpoint(0))),
            ((22, 5), Some(HitId::Pane(Pane::Input))),
            ((80, 5), None),
            ((0, 24), None),
        ];
        for ((col, row), want) in table {
            assert_eq!(m.at(col, row).map(|r| r.id), want, "({col}, {row})");
        }
    }

    #[test]
    fn a_modal_backdrop_is_above_everything_and_its_box_above_that() {
        let mut m = map();
        m.add(Rect::new(0, 0, 80, 24), HitId::Backdrop, HitKind::Modal, z::BACKDROP);
        m.add(Rect::new(10, 4, 60, 16), HitId::Modal, HitKind::Modal, z::MODAL);
        let table = [
            ((5, 17), HitId::Backdrop),
            ((10, 5), HitId::Modal),
            ((79, 18), HitId::Backdrop),
            ((40, 10), HitId::Modal),
            ((0, 0), HitId::Backdrop),
        ];
        for ((col, row), want) in table {
            assert_eq!(m.at(col, row).map(|r| r.id), Some(want), "({col}, {row})");
        }
        assert!(!m.hoverable(), "nothing behind a modal reacts to hover");
        m.clear();
        assert!(m.at(5, 5).is_none());
    }

    #[test]
    fn equal_layers_go_to_the_later_registration() {
        let mut m = HitMap::default();
        m.add(Rect::new(0, 0, 4, 4), HitId::Card(0), HitKind::Item, z::ITEM);
        m.add(Rect::new(2, 0, 4, 4), HitId::Card(1), HitKind::Item, z::ITEM);
        assert_eq!(m.at(1, 1).map(|r| r.id), Some(HitId::Card(0)));
        assert_eq!(m.at(3, 1).map(|r| r.id), Some(HitId::Card(1)));
        assert!(m.find(HitId::Card(1)).is_some());
        // An empty rectangle registers nothing.
        m.add(Rect::new(0, 0, 0, 5), HitId::Card(2), HitKind::Item, z::ITEM);
        assert!(m.find(HitId::Card(2)).is_none());
    }
}
