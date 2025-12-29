use gpui::{Global, Hsla};
use std::rc::Rc;

#[derive(Clone)]
pub struct ActiveTheme {
    pub name: &'static str,
    pub theme: Rc<Theme>,
}

impl ActiveTheme {
    pub fn light() -> Self {
        Self {
            name: "light",
            theme: Rc::new(Theme::light()),
        }
    }

    pub fn dark() -> Self {
        Self {
            name: "dark",
            theme: Rc::new(Theme::dark()),
        }
    }
}

impl Global for ActiveTheme {}

impl std::ops::Deref for ActiveTheme {
    type Target = Theme;

    fn deref(&self) -> &Self::Target {
        &self.theme
    }
}

pub struct Theme {
    pub primary: Hsla,
    pub primary_hover: Hsla,
    pub secondary: Hsla,
    pub secondary_hover: Hsla,
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub background: Hsla,
    pub surface: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub border: Hsla,
    pub card: Hsla,
    pub card_hover: Hsla,
    pub selected: Hsla,
    pub sidebar: Hsla,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            // Primary: Soft pink
            primary: Hsla {
                h: 330.0 / 360.0,
                s: 0.71,
                l: 0.85,
                a: 1.0,
            },
            primary_hover: Hsla {
                h: 340.0 / 360.0,
                s: 0.82,
                l: 0.60,
                a: 1.0,
            },
            // Secondary: Pastel purple
            secondary: Hsla {
                h: 291.0 / 360.0,
                s: 0.47,
                l: 0.71,
                a: 1.0,
            },
            secondary_hover: Hsla {
                h: 291.0 / 360.0,
                s: 0.64,
                l: 0.51,
                a: 1.0,
            },
            // Accent: Pastel blue
            accent: Hsla {
                h: 207.0 / 360.0,
                s: 0.89,
                l: 0.78,
                a: 1.0,
            },
            accent_hover: Hsla {
                h: 207.0 / 360.0,
                s: 0.90,
                l: 0.54,
                a: 1.0,
            },
            // Background: Warm white
            background: Hsla {
                h: 330.0 / 360.0,
                s: 1.0,
                l: 0.98,
                a: 1.0,
            },
            // Surface: Light lavender
            surface: Hsla {
                h: 300.0 / 360.0,
                s: 0.67,
                l: 0.94,
                a: 1.0,
            },
            // Text: Soft charcoal
            text: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.26,
                a: 1.0,
            },
            // Text: Muted (secondary text)
            text_muted: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.46,
                a: 1.0,
            },
            // Border color
            border: Hsla {
                h: 300.0 / 360.0,
                s: 0.30,
                l: 0.85,
                a: 1.0,
            },
            // Card background (slightly elevated)
            card: Hsla {
                h: 0.0,
                s: 0.0,
                l: 1.0,
                a: 1.0,
            },
            // Card hover state
            card_hover: Hsla {
                h: 330.0 / 360.0,
                s: 0.50,
                l: 0.97,
                a: 1.0,
            },
            // Selected state
            selected: Hsla {
                h: 330.0 / 360.0,
                s: 0.71,
                l: 0.92,
                a: 1.0,
            },
            // Sidebar background
            sidebar: Hsla {
                h: 300.0 / 360.0,
                s: 0.50,
                l: 0.96,
                a: 1.0,
            },
        }
    }

    pub fn dark() -> Self {
        Self {
            // Primary: Deeper pink
            primary: Hsla {
                h: 330.0 / 360.0,
                s: 0.60,
                l: 0.45,
                a: 1.0,
            },
            primary_hover: Hsla {
                h: 330.0 / 360.0,
                s: 0.65,
                l: 0.55,
                a: 1.0,
            },
            // Secondary: Deeper purple
            secondary: Hsla {
                h: 291.0 / 360.0,
                s: 0.40,
                l: 0.40,
                a: 1.0,
            },
            secondary_hover: Hsla {
                h: 291.0 / 360.0,
                s: 0.50,
                l: 0.50,
                a: 1.0,
            },
            // Accent: Deeper blue
            accent: Hsla {
                h: 207.0 / 360.0,
                s: 0.70,
                l: 0.45,
                a: 1.0,
            },
            accent_hover: Hsla {
                h: 207.0 / 360.0,
                s: 0.75,
                l: 0.55,
                a: 1.0,
            },
            // Background: Dark gray with slight warmth
            background: Hsla {
                h: 300.0 / 360.0,
                s: 0.10,
                l: 0.10,
                a: 1.0,
            },
            // Surface: Slightly elevated dark
            surface: Hsla {
                h: 300.0 / 360.0,
                s: 0.15,
                l: 0.15,
                a: 1.0,
            },
            // Text: Light gray
            text: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.90,
                a: 1.0,
            },
            // Text: Muted
            text_muted: Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.60,
                a: 1.0,
            },
            // Border color
            border: Hsla {
                h: 300.0 / 360.0,
                s: 0.15,
                l: 0.25,
                a: 1.0,
            },
            // Card background
            card: Hsla {
                h: 300.0 / 360.0,
                s: 0.10,
                l: 0.13,
                a: 1.0,
            },
            // Card hover state
            card_hover: Hsla {
                h: 300.0 / 360.0,
                s: 0.15,
                l: 0.18,
                a: 1.0,
            },
            // Selected state
            selected: Hsla {
                h: 330.0 / 360.0,
                s: 0.40,
                l: 0.25,
                a: 1.0,
            },
            // Sidebar background
            sidebar: Hsla {
                h: 300.0 / 360.0,
                s: 0.12,
                l: 0.08,
                a: 1.0,
            },
        }
    }
}
