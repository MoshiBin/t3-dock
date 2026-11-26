//! An allowance to show on the side display, such as an AI plan's monthly credits.
//!
//! t3-dock holds no credentials for it. Another program that already has them, such as an agent
//! extension, writes `usage.json` in the data folder:
//!
//! ```json
//! {"label": "ACU", "used": 1270.5, "limit": 1350}
//! ```
//!
//! Writers should write a temporary file and rename it. A missing or unreadable file shows nothing.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

pub const FILE: &str = "usage.json";
/// What fits on the banner before " LEFT".
const LABEL_LENGTH: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub label: String,
    pub used: f64,
    pub limit: f64,
}

#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    label: String,
    used: f64,
    limit: f64,
}

impl Usage {
    pub fn parse(text: &str) -> Option<Self> {
        let wire: Wire = serde_json::from_str(text).ok()?;
        if !wire.used.is_finite() || !wire.limit.is_finite() || wire.used < 0.0 || wire.limit <= 0.0
        {
            return None;
        }
        // Only letters and digits reach the key, uppercased like the other banners.
        let label: String = wire
            .label
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(LABEL_LENGTH)
            .collect::<String>()
            .to_ascii_uppercase();
        Some(Self {
            label,
            used: wire.used,
            limit: wire.limit,
        })
    }

    pub fn banner(&self) -> String {
        if self.label.is_empty() {
            "LEFT".to_owned()
        } else {
            format!("{} LEFT", self.label)
        }
    }
}

/// Notices when the file changes.
pub struct Watch {
    path: PathBuf,
    seen: Option<Option<SystemTime>>,
}

impl Watch {
    pub fn new(data: &Path) -> Self {
        Self {
            path: data.join(FILE),
            seen: None,
        }
    }

    /// The new contents, once per change and once at the start.
    pub fn changed(&mut self) -> Option<Option<Usage>> {
        let now = std::fs::metadata(&self.path)
            .and_then(|meta| meta.modified())
            .ok();
        if self.seen == Some(now) {
            return None;
        }
        self.seen = Some(now);
        Some(
            std::fs::read_to_string(&self.path)
                .ok()
                .and_then(|text| Usage::parse(&text)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_usage_file_is_checked() {
        let usage = Usage::parse(r#"{"label": "acu", "used": 1270.5, "limit": 1350}"#).unwrap();
        assert_eq!(usage.banner(), "ACU LEFT");
        assert_eq!((usage.used, usage.limit), (1270.5, 1350.0));
        let odd = Usage::parse(r#"{"label": "a\nvery long label", "used": 0, "limit": 1}"#);
        assert_eq!(odd.unwrap().banner(), "AVERY LEFT");
        assert_eq!(
            Usage::parse(r#"{"used": 1, "limit": 2}"#).unwrap().banner(),
            "LEFT"
        );
        for bad in [
            r#"{"used": 1, "limit": 0}"#,
            r#"{"used": -1, "limit": 10}"#,
            r#"{"used": 1}"#,
            "not json",
        ] {
            assert_eq!(Usage::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_watch_reports_changes_and_removal() {
        let folder = std::env::temp_dir().join(format!("t3-dock-usage-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let mut watch = Watch::new(&folder);
        assert_eq!(watch.changed(), Some(None));
        assert_eq!(watch.changed(), None);
        std::fs::write(folder.join(FILE), r#"{"used": 1, "limit": 2}"#).unwrap();
        assert_eq!(watch.changed().unwrap().unwrap().used, 1.0);
        assert_eq!(watch.changed(), None);
        std::fs::remove_file(folder.join(FILE)).unwrap();
        assert_eq!(watch.changed(), Some(None));
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
