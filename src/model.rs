//! The Stream Dock models t3-dock drives, and how each one numbers its keys.
//!
//! The rest of the app counts screen keys in reading order, from the top-left key, row by row.
//! Each model maps that order to its own image slots and input codes.
//!
//! Facts are from MiraBox's MIT SDK (`StreamDockN3.py`, `StreamDock293sV3.py` and the C++ SDK's
//! `streamdock293sV3.cpp`). See docs/HANDOFF.md for what was confirmed on the hardware.

use mirajazz::device::DeviceQuery;

/// How a model lays its keys out. The app only needs this, not the wire details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Pressable screen keys.
    pub keys: usize,
    /// Keys in a row.
    pub columns: usize,
    /// Tiles on a display that shows but cannot be pressed.
    pub side: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    /// 90 degrees clockwise.
    Clockwise,
    /// 90 degrees counterclockwise.
    Counterclockwise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// 6 screen keys in 2 rows, 3 keys without screens and 3 dials.
    N3,
    /// Stream Dock 293S V3: 15 screen keys in 3 rows and a narrow display of 3 tiles beside them.
    Dock293sV3,
}

const N3_QUERY: DeviceQuery = DeviceQuery::new(65440, 1, 0x6603, 0x1003);
const DOCK_293S_V3_QUERY: DeviceQuery = DeviceQuery::new(65440, 1, 0x6603, 0x1014);

pub const QUERIES: [DeviceQuery; 2] = [N3_QUERY, DOCK_293S_V3_QUERY];

impl Model {
    pub fn from_ids(vendor: u16, product: u16) -> Option<Self> {
        match (vendor, product) {
            (0x6603, 0x1003) => Some(Self::N3),
            (0x6603, 0x1014) => Some(Self::Dock293sV3),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::N3 => "N3",
            Self::Dock293sV3 => "293S",
        }
    }

    pub fn layout(self) -> Layout {
        match self {
            Self::N3 => Layout {
                keys: 6,
                columns: 3,
                side: 0,
            },
            Self::Dock293sV3 => Layout {
                keys: 15,
                columns: 5,
                side: 3,
            },
        }
    }

    /// `Device::connect`'s protocol version, key count and encoder count.
    pub fn connect_args(self) -> (usize, usize, usize) {
        match self {
            Self::N3 => (3, 9, 3),
            Self::Dock293sV3 => (3, 18, 0),
        }
    }

    /// Square images, in pixels.
    pub fn key_size(self) -> u32 {
        match self {
            Self::N3 => 64,
            Self::Dock293sV3 => 96,
        }
    }

    /// Side tiles are square too, and narrower than the keys.
    pub fn side_size(self) -> u32 {
        match self {
            Self::N3 => 0,
            Self::Dock293sV3 => 88,
        }
    }

    pub fn rotation(self) -> Rotation {
        match self {
            Self::N3 => Rotation::Clockwise,
            Self::Dock293sV3 => Rotation::Counterclockwise,
        }
    }

    /// The image slot of a screen key, 0-based as mirajazz wants it.
    pub fn key_slot(self, key: usize) -> u8 {
        match self {
            Self::N3 => key as u8,
            // Slots run down each column, starting with the right-hand column.
            Self::Dock293sV3 => {
                let (row, column) = (key / 5, key % 5);
                ((4 - column) * 3 + row) as u8
            }
        }
    }

    /// The image slot of a side tile, from the top, 0-based.
    pub fn side_slot(self, tile: usize) -> u8 {
        match self {
            Self::N3 => unreachable!("the N3 has no side display"),
            Self::Dock293sV3 => 15 + tile as u8,
        }
    }

    /// The screen key, in reading order, that an input code reports.
    pub fn screen_key(self, code: u8) -> Option<usize> {
        match (self, code) {
            (Self::N3, 0x01..=0x06) => Some(usize::from(code - 1)),
            // Input codes number the keys as the image slots do, 1-based.
            (Self::Dock293sV3, 0x01..=0x0f) => {
                let slot = usize::from(code - 1);
                let (column, row) = (4 - slot / 3, slot % 3);
                Some(row * 5 + column)
            }
            _ => None,
        }
    }

    /// Whether the model has the N3's keys without screens and dials.
    pub fn has_controls(self) -> bool {
        self == Self::N3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_its_own_slot_and_code() {
        for model in [Model::N3, Model::Dock293sV3] {
            let layout = model.layout();
            let mut slots: Vec<u8> = (0..layout.keys).map(|key| model.key_slot(key)).collect();
            slots.extend((0..layout.side).map(|tile| model.side_slot(tile)));
            slots.sort_unstable();
            slots.dedup();
            assert_eq!(slots.len(), layout.keys + layout.side, "{model:?}");
            for key in 0..layout.keys {
                assert_eq!(model.screen_key(model.key_slot(key) + 1), Some(key));
            }
        }
    }

    #[test]
    fn the_293s_counts_down_columns_from_the_right() {
        let model = Model::Dock293sV3;
        // Top-right, the key below it, and the top-left.
        assert_eq!(model.key_slot(4), 0);
        assert_eq!(model.key_slot(9), 1);
        assert_eq!(model.key_slot(0), 12);
        assert_eq!(model.key_slot(14), 2);
        assert_eq!(model.screen_key(0x0d), Some(0));
        assert_eq!(model.screen_key(0x10), None);
    }

    #[test]
    fn models_are_known_by_their_ids() {
        assert_eq!(Model::from_ids(0x6603, 0x1003), Some(Model::N3));
        assert_eq!(Model::from_ids(0x6603, 0x1014), Some(Model::Dock293sV3));
        assert_eq!(Model::from_ids(0x6603, 0x1005), None);
    }
}
