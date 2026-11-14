//! The dashboard frame: rectangles from the row budget (05 sections 1 and 6), the two borders
//! the user may drag (6.1), and the sizes kept in `state/ui.toml`.

use std::io;
use std::path::Path;

use ratatui::layout::Rect;

/// The smallest terminal the dashboard draws in. Below it the frame is a message.
pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 24;
/// Under this many columns the rail is a strip; under this many rows the log has 3 rows and the
/// fork layer loses its footer.
pub const RAIL_COLS: u16 = 98;
pub const COMPACT_ROWS: u16 = 30;
/// The status bar shows the ledger segment from this width.
pub const LEDGER_COLS: u16 = 100;
/// The rail's width when drawn in full, and the least a dragged border may leave it.
pub const RAIL_FULL: u16 = 22;
const STRIP: u16 = 3;
/// The centre column (the 40-column boxes) is never squeezed below this.
const CENTRE_MIN: u16 = 40;
/// The log box: border plus the 3 rows of a compact terminal, and the main area keeps this many.
const LOG_MIN: u16 = 5;
const MAIN_MIN: u16 = 14;

/// Sizes the user dragged; `None` is the automatic size for the terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// Width of the full rail.
    pub rail: Option<u16>,
    /// Height of the log box, border included.
    pub log: Option<u16>,
}

/// The rectangles of one frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Areas {
    pub header: Rect,
    pub legend: Rect,
    /// The 3-column strip, or the full rail when the terminal is wide enough.
    pub rail: Rect,
    pub centre: Rect,
    pub log: Rect,
    pub prompt: Rect,
    pub status: Rect,
    /// The full rail drawn over the strip after `a`, when the terminal is too narrow for it.
    pub rail_open: Option<Rect>,
    /// The column between the rail and the centre; only when the full rail sits beside it.
    pub rail_handle: Option<Rect>,
    /// The log's top border row.
    pub log_handle: Rect,
    /// Under 30 rows: 3 log rows, no fork footer.
    pub compact: bool,
}

pub fn too_small(full: Rect) -> bool {
    full.width < MIN_COLS || full.height < MIN_ROWS
}

impl Layout {
    /// The rail width this layout gives a terminal `cols` wide.
    pub fn rail_width(&self, cols: u16) -> u16 {
        let most = cols.saturating_sub(4 + CENTRE_MIN).clamp(RAIL_FULL, 40);
        self.rail.unwrap_or(RAIL_FULL).clamp(RAIL_FULL, most)
    }

    pub fn from_toml(text: &str) -> Self {
        let t: toml::Table = text.parse().unwrap_or_default();
        let get = |k: &str| {
            t.get(k)
                .and_then(toml::Value::as_integer)
                .and_then(|v| u16::try_from(v).ok())
        };
        Self {
            rail: get("rail"),
            log: get("log"),
        }
    }

    pub fn to_toml(&self) -> String {
        let mut out = String::from("# Sizes dragged in the dashboard (05 section 6.1).\n");
        for (k, v) in [("rail", self.rail), ("log", self.log)] {
            if let Some(v) = v {
                out.push_str(&format!("{k} = {v}\n"));
            }
        }
        out
    }

    /// Reads `state/ui.toml`; a missing or unreadable file is the automatic layout.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|t| Self::from_toml(&t))
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            aibus_core::persist::fs::create_private_dir(dir)?;
        }
        aibus_core::persist::fs::write_private(path, self.to_toml())
    }
}

/// Row budget at 46 rows: header 2, legend 1, main 30, log 6, prompt 1, status 1, and five
/// spacing rows. Taller terminals give the extra rows to the log; shorter ones lose spacing
/// first, then main rows. Under 30 rows the log keeps 3 rows. Under 98 columns the rail is a
/// 3-column strip. `layout` holds the sizes the user dragged, clamped to the minimums here.
pub fn areas(full: Rect, layout: &Layout, rail_open: bool) -> Areas {
    let rows = full.height;
    let compact = rows < COMPACT_ROWS;
    let base_log = if compact { 5 } else { 6 };
    let gaps = rows.saturating_sub(5 + base_log + 30).min(5);
    let auto_log = base_log + rows.saturating_sub(46);
    let most_log = rows.saturating_sub(5 + gaps + MAIN_MIN).max(LOG_MIN);
    let log_h = layout.log.map_or(auto_log, |h| h.clamp(LOG_MIN, most_log));
    let main_h = rows.saturating_sub(5 + log_h + gaps);
    let gap = |i: u16| u16::from(gaps > i);
    // Gap order: under the legend, under main, under the log, under the header, over status.
    let mut y = full.y;
    let mut take = |h: u16, after: u16| {
        let r = Rect {
            y,
            height: h,
            x: full.x,
            width: full.width,
        };
        y += h + after;
        r
    };
    let header = take(2, gap(3));
    let legend = take(1, gap(0));
    let main = take(main_h, gap(1));
    let log = take(log_h, gap(2));
    let prompt = take(1, gap(4));
    let status = take(1, 0);
    let wide = full.width >= RAIL_COLS;
    let rail_w = if wide {
        layout.rail_width(full.width)
    } else {
        STRIP
    };
    let rail = Rect {
        x: main.x + 1,
        width: rail_w,
        ..main
    };
    let cx = rail.right() + 2;
    let centre = Rect {
        x: cx,
        width: main.right().saturating_sub(cx + 1),
        ..main
    };
    let fit = |r: Rect| r.intersection(full);
    Areas {
        header: fit(header),
        legend: fit(legend),
        rail: fit(rail),
        centre: fit(centre),
        log: fit(log),
        prompt: fit(prompt),
        status: fit(status),
        rail_open: (!wide && rail_open).then(|| {
            fit(Rect {
                width: RAIL_FULL,
                ..rail
            })
        }),
        rail_handle: wide.then(|| {
            fit(Rect {
                x: rail.right(),
                width: 1,
                ..main
            })
        }),
        log_handle: fit(Rect { height: 1, ..log }),
        compact,
    }
}

/// The rail width a drag to column `col` asks for.
pub fn rail_for_drag(a: &Areas, col: u16, cols: u16) -> u16 {
    Layout {
        rail: Some(col.saturating_sub(a.rail.x)),
        log: None,
    }
    .rail_width(cols)
}

/// The log height (border included) a drag to row `row` asks for.
pub fn log_for_drag(a: &Areas, row: u16) -> u16 {
    a.log.bottom().saturating_sub(row).max(LOG_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO: Layout = Layout {
        rail: None,
        log: None,
    };

    fn at(w: u16, h: u16) -> Areas {
        areas(Rect::new(0, 0, w, h), &AUTO, false)
    }

    #[test]
    fn the_rail_is_a_strip_under_98_columns_and_a_full_rail_from_98() {
        assert_eq!(at(97, 46).rail.width, 3);
        assert_eq!(at(98, 46).rail.width, 22);
        assert!(at(97, 46).rail_handle.is_none());
        assert!(at(98, 46).rail_handle.is_some());
    }

    #[test]
    fn a_is_the_full_rail_over_the_strip_on_a_narrow_terminal() {
        let a = areas(Rect::new(0, 0, 80, 24), &AUTO, true);
        let open = a.rail_open.unwrap();
        assert_eq!(
            (open.x, open.width, open.height),
            (a.rail.x, 22, a.rail.height)
        );
        assert!(areas(Rect::new(0, 0, 98, 46), &AUTO, true)
            .rail_open
            .is_none());
    }

    #[test]
    fn the_log_has_three_rows_under_30_rows_and_four_from_30() {
        assert_eq!(at(98, 29).log.height, 5);
        assert!(at(98, 29).compact);
        assert_eq!(at(98, 30).log.height, 6);
        assert!(!at(98, 30).compact);
        assert_eq!(at(98, 46).log.height, 6);
        assert_eq!(at(120, 50).log.height, 10);
    }

    #[test]
    fn below_80x24_is_too_small() {
        assert!(too_small(Rect::new(0, 0, 79, 24)));
        assert!(too_small(Rect::new(0, 0, 80, 23)));
        assert!(!too_small(Rect::new(0, 0, 80, 24)));
    }

    #[test]
    fn dragged_sizes_are_clamped_to_the_minimums() {
        let full = Rect::new(0, 0, 120, 50);
        let wide = areas(
            full,
            &Layout {
                rail: Some(500),
                log: Some(2),
            },
            false,
        );
        assert_eq!(wide.rail.width, 40);
        assert_eq!(wide.log.height, 5);
        let narrow = areas(
            full,
            &Layout {
                rail: Some(3),
                log: Some(500),
            },
            false,
        );
        assert_eq!(narrow.rail.width, 22);
        assert!(narrow.centre.height >= 14, "{:?}", narrow.centre);
        // A terminal just wide enough leaves the centre its 40 columns.
        let tight = areas(
            Rect::new(0, 0, 98, 46),
            &Layout {
                rail: Some(60),
                log: None,
            },
            false,
        );
        assert!(tight.centre.width >= 40 - 1, "{:?}", tight.centre);
    }

    #[test]
    fn drags_map_a_pointer_cell_to_a_size() {
        let a = at(120, 50);
        assert_eq!(rail_for_drag(&a, a.rail.x + 30, 120), 30);
        assert_eq!(rail_for_drag(&a, 0, 120), 22);
        assert_eq!(log_for_drag(&a, a.log.y - 3), a.log.height + 3);
        assert_eq!(log_for_drag(&a, a.log.bottom()), 5);
    }

    #[test]
    fn ui_toml_round_trips_and_forgives() {
        let l = Layout {
            rail: Some(30),
            log: Some(9),
        };
        assert_eq!(Layout::from_toml(&l.to_toml()), l);
        assert_eq!(Layout::from_toml(&AUTO.to_toml()), AUTO);
        assert_eq!(Layout::from_toml("rail = \"wide\"\nlog = -4\n"), AUTO);
        assert_eq!(Layout::from_toml("not toml ["), AUTO);
        let dir = std::env::temp_dir().join(format!("aibus-ui-toml-{}", std::process::id()));
        let path = dir.join("state").join("ui.toml");
        l.save(&path).unwrap();
        assert_eq!(Layout::load(&path), l);
        assert_eq!(Layout::load(&dir.join("missing.toml")), AUTO);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
