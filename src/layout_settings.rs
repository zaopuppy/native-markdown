use crate::{appearance::Appearance, markdown::Dialect, paste::DownloadTimeouts};
use std::fs;
use std::io;
use std::path::PathBuf;

const DEFAULT_FILE_TREE_WIDTH: f32 = 280.0;
const DEFAULT_OUTLINE_WIDTH: f32 = 270.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutSettings {
    pub appearance: Appearance,
    pub file_tree_open: bool,
    pub outline_open: bool,
    pub file_tree_width: f32,
    pub outline_width: f32,
    pub reading_width: u16,
    pub reading_width_follows_zoom: bool,
    pub preview_refresh_delay_ms: u64,
    pub markdown_dialect: Dialect,
    pub preserve_line_breaks: bool,
    pub download_timeouts: DownloadTimeouts,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            file_tree_open: true,
            outline_open: true,
            file_tree_width: DEFAULT_FILE_TREE_WIDTH,
            outline_width: DEFAULT_OUTLINE_WIDTH,
            reading_width: 980,
            reading_width_follows_zoom: true,
            preview_refresh_delay_ms: 1000,
            markdown_dialect: Dialect::default(),
            preserve_line_breaks: false,
            download_timeouts: DownloadTimeouts::default(),
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
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "configuration directory unavailable",
            ));
        };
        self.save_to(&path)
    }

    fn save_to(&self, path: &std::path::Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::document::atomic_write(path, self.serialize().as_bytes())
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
                "theme" => settings.appearance = Appearance::parse(value),
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
                "reading_width" => {
                    settings.reading_width =
                        choice(value, &[0, 720, 980, 1280], settings.reading_width as u64) as u16
                }
                "reading_width_follows_zoom" => {
                    settings.reading_width_follows_zoom =
                        parse_bool(value).unwrap_or(settings.reading_width_follows_zoom)
                }
                "preview_refresh_delay_ms" => {
                    settings.preview_refresh_delay_ms = choice(
                        value,
                        &[500, 1000, 2000, 3000, 5000],
                        settings.preview_refresh_delay_ms,
                    )
                }
                "markdown_dialect" => {
                    settings.markdown_dialect = match value.trim() {
                        "commonmark" => Dialect::CommonMark,
                        _ => Dialect::GitHub,
                    }
                }
                "preserve_line_breaks" => {
                    settings.preserve_line_breaks =
                        parse_bool(value).unwrap_or(settings.preserve_line_breaks)
                }
                "download_connect_secs" => {
                    settings.download_timeouts.connect_secs = choice(
                        value,
                        &[3, 5, 10, 15, 30],
                        settings.download_timeouts.connect_secs,
                    )
                }
                "download_image_secs" => {
                    settings.download_timeouts.image_secs = choice(
                        value,
                        &[5, 10, 20, 30, 60],
                        settings.download_timeouts.image_secs,
                    )
                }
                "download_batch_secs" => {
                    settings.download_timeouts.batch_secs = choice(
                        value,
                        &[10, 20, 30, 60, 120],
                        settings.download_timeouts.batch_secs,
                    )
                }
                _ => {}
            }
        }
        settings
    }

    fn serialize(self) -> String {
        format!(
            "file_tree_open={}\noutline_open={}\nfile_tree_width={}\noutline_width={}\ntheme={}\nreading_width={}\nreading_width_follows_zoom={}\npreview_refresh_delay_ms={}\nmarkdown_dialect={}\npreserve_line_breaks={}\ndownload_connect_secs={}\ndownload_image_secs={}\ndownload_batch_secs={}\n",
            self.file_tree_open,
            self.outline_open,
            self.file_tree_width,
            self.outline_width,
            self.appearance.id(),
            self.reading_width,
            self.reading_width_follows_zoom,
            self.preview_refresh_delay_ms,
            self.markdown_dialect.id(),
            self.preserve_line_breaks,
            self.download_timeouts.connect_secs,
            self.download_timeouts.image_secs,
            self.download_timeouts.batch_secs
        )
    }
}

fn choice(value: &str, allowed: &[u64], fallback: u64) -> u64 {
    value
        .trim()
        .parse()
        .ok()
        .filter(|value| allowed.contains(value))
        .unwrap_or(fallback)
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
    fn preferences_survive_file_save_and_reload_and_reject_invalid_limits() {
        let folder = tempfile::tempdir_in("tmp").unwrap();
        let path = folder.path().join("layout.conf");
        let settings = LayoutSettings {
            reading_width: 1280,
            reading_width_follows_zoom: false,
            preview_refresh_delay_ms: 3000,
            markdown_dialect: Dialect::CommonMark,
            preserve_line_breaks: true,
            download_timeouts: DownloadTimeouts {
                connect_secs: 15,
                image_secs: 30,
                batch_secs: 60,
            },
            ..LayoutSettings::default()
        };
        settings.save_to(&path).unwrap();
        assert_eq!(
            LayoutSettings::parse(&fs::read_to_string(&path).unwrap()),
            settings
        );
        LayoutSettings::default().save_to(&path).unwrap();
        assert_eq!(
            LayoutSettings::parse(&fs::read_to_string(&path).unwrap()),
            LayoutSettings::default()
        );
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 1);
        assert!(settings.save_to(folder.path()).is_err());
        assert_eq!(LayoutSettings::parse("download_connect_secs=0\ndownload_image_secs=-1\ndownload_batch_secs=18446744073709551615\nreading_width=42\npreview_refresh_delay_ms=oops\nmarkdown_dialect=unknown\npreserve_line_breaks=invalid\nreading_width_follows_zoom=invalid\n"), LayoutSettings::default());
        assert_eq!(
            LayoutSettings::parse("theme=paper\n"),
            LayoutSettings::default()
        );
    }

    #[test]
    fn themes_round_trip_with_layout_and_unknown_themes_fall_back() {
        for appearance in Appearance::ALL {
            let settings = LayoutSettings {
                appearance,
                file_tree_open: false,
                ..LayoutSettings::default()
            };
            assert_eq!(LayoutSettings::parse(&settings.serialize()), settings);
        }
        assert_eq!(
            LayoutSettings::parse("theme=removed-theme\n").appearance,
            Appearance::Paper
        );
        assert_eq!(
            LayoutSettings::parse("outline_open=false\n").appearance,
            Appearance::Paper
        );
    }

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
