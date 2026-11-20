use gpui::{px, rgb, App};
use gpui_component::{Theme, ThemeMode};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Paper,
    Sepia,
    Sage,
    Midnight,
    Nord,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: u32,
    pub surface: u32,
    pub panel: u32,
    pub text: u32,
    pub muted: u32,
    pub border: u32,
    pub selection: u32,
    pub hover: u32,
    pub accent: u32,
    pub link: u32,
    pub danger: u32,
    pub warning: u32,
}

impl Appearance {
    pub const ALL: [Self; 5] = [
        Self::Paper,
        Self::Sepia,
        Self::Sage,
        Self::Midnight,
        Self::Nord,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::Sepia => "sepia",
            Self::Sage => "sage",
            Self::Midnight => "midnight",
            Self::Nord => "nord",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Paper => "Paper · 纸白",
            Self::Sepia => "Sepia · 暖纸",
            Self::Sage => "Sage · 鼠尾草",
            Self::Midnight => "Midnight · 午夜",
            Self::Nord => "Nord · 北欧",
        }
    }

    pub fn parse(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|theme| theme.id() == value.trim())
            .unwrap_or_default()
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Self::Midnight | Self::Nord)
    }

    pub fn palette(self) -> Palette {
        let (canvas, surface, panel, text, muted, border, selection, hover, accent, link) =
            match self {
                Self::Paper => (
                    0xfafbf8, 0xf8f9f6, 0xeeefec, 0x292e2b, 0x656b67, 0xdde0db, 0xdfe7de, 0xe7ece5,
                    0x285547, 0x38658a,
                ),
                Self::Sepia => (
                    0xf8f1e3, 0xf1e8d8, 0xece1cc, 0x443b30, 0x766957, 0xd9ccb5, 0xe2d2b4, 0xeee2cc,
                    0x795632, 0x82623a,
                ),
                Self::Sage => (
                    0xf0f5ef, 0xe8efe6, 0xe1eade, 0x2c3b30, 0x5c705e, 0xcbd8c7, 0xcddfc8, 0xdce8d8,
                    0x356144, 0x386859,
                ),
                Self::Midnight => (
                    0x202329, 0x262a32, 0x2d323c, 0xe2e5eb, 0xa4acba, 0x414856, 0x3c4d65, 0x343e4d,
                    0xa8c7f0, 0x9cc4f1,
                ),
                Self::Nord => (
                    0x2e3440, 0x333b49, 0x3b4252, 0xe5e9f0, 0xb0bdcf, 0x4c566a, 0x465c6d, 0x414d60,
                    0x8fbcbb, 0x88c0d0,
                ),
            };
        Palette {
            canvas,
            surface,
            panel,
            text,
            muted,
            border,
            selection,
            hover,
            accent,
            link,
            danger: if self.is_dark() { 0xf0a39b } else { 0x9b392a },
            warning: if self.is_dark() { 0xebcb8b } else { 0x95671d },
        }
    }

    pub fn apply(self, cx: &mut App) {
        Theme::change(
            if self.is_dark() {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            cx,
        );
        let p = self.palette();
        Theme::update(cx, |theme| {
            theme.font_family = ".SystemUIFont".into();
            theme.font_size = px(16.0);
            theme.mono_font_family = "Consolas".into();
            theme.mono_font_size = px(13.0);
            let colors = &mut theme.colors;
            colors.background = rgb(p.canvas).into();
            colors.foreground = rgb(p.text).into();
            colors.border = rgb(p.border).into();
            colors.input = rgb(p.canvas).into();
            colors.caret = rgb(p.accent).into();
            colors.selection = rgb(p.selection).into();
            colors.accent = rgb(p.hover).into();
            colors.accent_foreground = rgb(p.accent).into();
            colors.popover = rgb(p.canvas).into();
            colors.popover_foreground = rgb(p.text).into();
            colors.muted = rgb(p.panel).into();
            colors.muted_foreground = rgb(p.muted).into();
            colors.secondary = rgb(p.panel).into();
            colors.secondary_hover = rgb(p.hover).into();
            colors.secondary_active = rgb(p.selection).into();
            colors.secondary_foreground = rgb(p.text).into();
            colors.ring = rgb(p.accent).into();
        });
    }

    #[cfg(windows)]
    pub fn reader_settings(self) -> serde_json::Value {
        let p = self.palette();
        serde_json::json!({"id":self.id(), "dark":self.is_dark(), "colors": {
            "canvas":format!("#{:06x}",p.canvas), "surface":format!("#{:06x}",p.surface),
            "panel":format!("#{:06x}",p.panel), "text":format!("#{:06x}",p.text),
            "muted":format!("#{:06x}",p.muted), "border":format!("#{:06x}",p.border),
            "selection":format!("#{:06x}",p.selection), "accent":format!("#{:06x}",p.accent),
            "link":format!("#{:06x}",p.link), "danger":format!("#{:06x}",p.danger)
        }})
    }
}
