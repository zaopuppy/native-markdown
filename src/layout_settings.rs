use std::fs;
use std::io;
use std::path::PathBuf;

const DEFAULT_FILE_TREE_WIDTH: f32 = 280.0;
const DEFAULT_OUTLINE_WIDTH: f32 = 270.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutSettings {
    pub file_tree_open: bool,
    pub outline_open: bool,
    pub file_tree_width: f32,
    pub outline_width: f32,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            file_tree_open: true,
            outline_open: true,
            file_tree_width: DEFAULT_FILE_TREE_WIDTH,
            outline_width: DEFAULT_OUTLINE_WIDTH,
        }
    }
}

impl LayoutSettings {
    #[cfg_attr(test, allow(dead_code))]
    pub fn load() -> Self {
        settings_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map_or_else(Self::default, |text| Self::parse(&text))
    }

    pub fn save(&self) -> io::Result<()> {
        let Some(path) = settings_path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, self.serialize())
    }

    pub fn set_widths(&mut self, file_tree_width: f32, outline_width: f32) {
        self.file_tree_width = clamp_width(file_tree_width, DEFAULT_FILE_TREE_WIDTH);
        self.outline_width = clamp_width(outline_width, DEFAULT_OUTLINE_WIDTH);
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "file_tree_open" => {
                    settings.file_tree_open = parse_bool(value).unwrap_or(settings.file_tree_open)
                }
                "outline_open" => {
                    settings.outline_open = parse_bool(value).unwrap_or(settings.outline_open)
                }
                "file_tree_width" => {
                    settings.file_tree_width = value
                        .trim()
                        .parse()
                        .ok()
                        .map_or(settings.file_tree_width, |value| {
                            clamp_width(value, DEFAULT_FILE_TREE_WIDTH)
                        })
                }
                "outline_width" => {
                    settings.outline_width = value
                        .trim()
                        .parse()
                        .ok()
                        .map_or(settings.outline_width, |value| {
                            clamp_width(value, DEFAULT_OUTLINE_WIDTH)
                        })
                }
                _ => {}
            }
        }
        settings
    }

    fn serialize(self) -> String {
        format!(
            "file_tree_open={}\noutline_open={}\nfile_tree_width={}\noutline_width={}\n",
            self.file_tree_open, self.outline_open, self.file_tree_width, self.outline_width
        )
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn clamp_width(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(120.0, 520.0)
    } else {
        fallback
    }
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.map(|base| base.join("NativeMarkdown").join("layout.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_backward_compatible_and_clamps_widths() {
        let settings = LayoutSettings::parse(
            "file_tree_open=false\noutline_open=true\nfile_tree_width=50\noutline_width=900\nfuture=value\n",
        );
        assert!(!settings.file_tree_open);
        assert!(settings.outline_open);
        assert_eq!(settings.file_tree_width, 120.0);
        assert_eq!(settings.outline_width, 520.0);
    }

    #[test]
    fn malformed_values_fall_back_to_defaults() {
        let settings = LayoutSettings::parse(
            "file_tree_open=perhaps\noutline_open=false\nfile_tree_width=NaN\n",
        );
        assert!(settings.file_tree_open);
        assert!(!settings.outline_open);
        assert_eq!(settings.file_tree_width, DEFAULT_FILE_TREE_WIDTH);
    }
}
