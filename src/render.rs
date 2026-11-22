//! Draws key tiles, upright. The device layer rotates them.

use cosmic_text::{
    Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
};
use image::{Rgb, RgbImage};

use crate::status::Status;

pub const SIZE: u32 = 64;
const BANNER: u32 = 15;

const DARK: [u8; 3] = [12, 12, 16];
const LIGHT: [u8; 3] = [240, 240, 240];
const AMBER: [u8; 3] = [255, 176, 32];
const BLUE: [u8; 3] = [60, 140, 255];
const GREEN: [u8; 3] = [50, 200, 110];
const RED: [u8; 3] = [235, 70, 70];
const GRAY: [u8; 3] = [130, 130, 140];

/// Everything that affects one key's pixels. No `Debug`, since it holds a title.
#[derive(Clone, PartialEq, Eq)]
pub struct Tile {
    banner: String,
    body: String,
    accent: [u8; 3],
    large: bool,
    pub pressed: bool,
}

impl Tile {
    pub fn thread(title: &str, status: Status) -> Self {
        let (banner, accent) = match status {
            Status::Approval => ("APPROVE", AMBER),
            Status::Input => ("INPUT", AMBER),
            Status::Plan => ("PLAN", AMBER),
            Status::Limited => ("LIMIT", RED),
            Status::Failed => ("FAILED", RED),
            Status::Starting => ("STARTING", BLUE),
            Status::Working => ("WORKING", BLUE),
            Status::Waiting => ("WAITING", BLUE),
            Status::Done => ("DONE", GREEN),
            Status::Monitoring => ("MONITOR", GRAY),
            Status::Idle => ("IDLE", GRAY),
        };
        // Titles are free text, so fold newlines and control characters into spaces.
        let body = title
            .split(|c: char| c.is_whitespace() || c.is_control())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            banner: banner.into(),
            body: if body.is_empty() {
                "Untitled".into()
            } else {
                body
            },
            accent,
            large: false,
            pressed: false,
        }
    }

    pub fn notice(banner: &str, body: &str) -> Self {
        Self {
            banner: banner.into(),
            body: body.into(),
            accent: GRAY,
            large: false,
            pressed: false,
        }
    }

    /// A numbered tile that shows image index and which way is up.
    pub fn numbered(number: usize) -> Self {
        Self {
            banner: format!("KEY {number}"),
            body: number.to_string(),
            accent: GRAY,
            large: true,
            pressed: false,
        }
    }
}

struct Text<'a> {
    value: &'a str,
    size: f32,
    line_height: f32,
    lines: usize,
    top: i32,
    color: [u8; 3],
    weight: Weight,
    align: Option<Align>,
}

pub struct Renderer {
    fonts: FontSystem,
    cache: SwashCache,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            fonts: FontSystem::new(),
            cache: SwashCache::new(),
        }
    }

    pub fn render(&mut self, tile: &Tile) -> RgbImage {
        let (background, ink) = if tile.pressed {
            (LIGHT, DARK)
        } else {
            (DARK, LIGHT)
        };
        let mut image = RgbImage::from_fn(SIZE, SIZE, |_, y| {
            Rgb(if y < BANNER { tile.accent } else { background })
        });
        self.draw(
            &mut image,
            Text {
                value: &tile.banner,
                size: 10.0,
                line_height: 13.0,
                lines: 1,
                top: 1,
                color: DARK,
                weight: Weight::BOLD,
                align: Some(Align::Center),
            },
        );
        let (size, line_height, lines, weight, align) = if tile.large {
            (34.0, 44.0, 1, Weight::BOLD, Some(Align::Center))
        } else {
            (12.0, 15.0, 3, Weight::NORMAL, None)
        };
        self.draw(
            &mut image,
            Text {
                value: &tile.body,
                size,
                line_height,
                lines,
                top: 17,
                color: ink,
                weight,
                align,
            },
        );
        image
    }

    fn draw(&mut self, image: &mut RgbImage, text: Text<'_>) {
        const MARGIN: i32 = 3;
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(text.size, text.line_height));
        buffer.set_size(Some((SIZE as i32 - 2 * MARGIN) as f32), None);
        let attrs = Attrs::new()
            .family(Family::Name("Segoe UI"))
            .weight(text.weight);
        fit(
            &mut self.fonts,
            &mut buffer,
            text.value,
            &attrs,
            text.align,
            text.lines,
        );

        let [r, g, b] = text.color;
        buffer.draw(
            &mut self.fonts,
            &mut self.cache,
            Color::rgb(r, g, b),
            |x, y, width, height, color| {
                let alpha = u32::from(color.a());
                let source = [color.r(), color.g(), color.b()];
                for y in y..y + height as i32 {
                    for x in x..x + width as i32 {
                        let (x, y) = (x + MARGIN, y + text.top);
                        if !(0..SIZE as i32).contains(&x) || !(0..SIZE as i32).contains(&y) {
                            continue;
                        }
                        let pixel = image.get_pixel_mut(x as u32, y as u32);
                        for (to, from) in pixel.0.iter_mut().zip(source) {
                            *to = ((u32::from(from) * alpha + u32::from(*to) * (255 - alpha)) / 255)
                                as u8;
                        }
                    }
                }
            },
        );
    }
}

/// Lays out `value` in at most `lines` lines, cutting it with an ellipsis if needed.
///
/// cosmic-text can ellipsize, but then starts the last line with the space it wrapped at.
fn fit(
    fonts: &mut FontSystem,
    buffer: &mut Buffer,
    value: &str,
    attrs: &Attrs<'_>,
    align: Option<Align>,
    lines: usize,
) {
    /// More than a key can show.
    const LIMIT: usize = 80;

    let mut fits = |text: &str| {
        buffer.set_text(text, attrs, Shaping::Advanced, align);
        buffer.shape_until_scroll(fonts, false);
        buffer.layout_runs().count() <= lines
    };
    if value.chars().count() <= LIMIT && fits(value) {
        return;
    }
    let chars: Vec<char> = value.chars().take(LIMIT).collect();
    let shortened = |length: usize| {
        let prefix: String = chars[..length].iter().collect();
        format!("{}…", prefix.trim_end())
    };
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(&shortened(middle)) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    fits(&shortened(low));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_is_key_sized_with_the_banner_on_top() {
        let image = Renderer::new().render(&Tile::thread("Invented title", Status::Failed));
        assert_eq!(image.dimensions(), (SIZE, SIZE));
        assert_eq!(image.get_pixel(0, 0).0, RED);
        assert_eq!(image.get_pixel(0, SIZE - 1).0, DARK);
    }

    #[test]
    fn titles_are_flattened() {
        assert_eq!(
            Tile::thread(" two\n\tlines ", Status::Idle).body,
            "two lines"
        );
        assert_eq!(Tile::thread(" \n", Status::Idle).body, "Untitled");
    }

    /// `cargo test preview -- --ignored` writes target/tiles.png for a look at the real renderer.
    #[test]
    #[ignore = "writes a preview image"]
    fn preview() {
        let mut pressed = Tile::numbered(6);
        pressed.pressed = true;
        let tiles = [
            Tile::thread("Speed up the invented parser benchmark", Status::Working),
            Tile::thread("Approve database migration", Status::Approval),
            Tile::thread("תיקון באג במסך Login של האתר", Status::Done),
            Tile::thread(
                "A very long invented title that cannot possibly fit on one small key",
                Status::Failed,
            ),
            Tile::notice("NO TOKEN", "Run t3-dock auth"),
            pressed,
        ];
        let mut renderer = Renderer::new();
        let mut sheet = RgbImage::from_pixel(3 * 208 + 16, 2 * 208 + 16, Rgb([55, 55, 58]));
        for (i, tile) in tiles.iter().enumerate() {
            let key = renderer.render(tile);
            let zoomed =
                image::imageops::resize(&key, 192, 192, image::imageops::FilterType::Nearest);
            let (x, y) = (16 + i % 3 * 208, 16 + i / 3 * 208);
            image::imageops::replace(&mut sheet, &zoomed, x as i64, y as i64);
        }
        sheet.save("target/tiles.png").unwrap();
    }
}
