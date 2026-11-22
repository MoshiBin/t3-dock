//! Feasibility probe: opens the N3 without sending it a report, and renders sample tiles.
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use image::{Rgb, RgbImage, imageops};
use mirajazz::device::{Device, DeviceQuery, list_devices};

const KEY: u32 = 64;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::args().nth(1).ok_or("usage: probe <output.png>")?;

    let found = list_devices(&[DeviceQuery::new(65440, 1, 0x6603, 0x1003)]).await?;
    println!("hid: {} device(s)", found.len());
    for info in &found {
        Device::connect(info, 3, 9, 3).await?;
        println!("hid: opened");
    }

    let mut fonts = FontSystem::new();
    let mut cache = SwashCache::new();
    let tiles = [
        ("Refactor the parser module", [60, 140, 255]),
        ("Approve database migration", [255, 176, 32]),
        ("תיקון באג במסך Login של האתר", [50, 200, 110]),
        ("Update dependencies", [50, 200, 110]),
        ("Flaky integration test", [235, 70, 70]),
        ("Write release notes", [50, 200, 110]),
    ];
    let mut sheet = RgbImage::from_pixel(3 * 208 + 16, 2 * 208 + 16, Rgb([55, 55, 58]));
    for (i, (title, accent)) in tiles.iter().enumerate() {
        let mut tile = RgbImage::from_pixel(KEY, KEY, Rgb([12, 12, 16]));
        for y in 0..6 {
            for x in 0..KEY {
                tile.put_pixel(x, y, Rgb(*accent));
            }
        }

        let mut text = Buffer::new(&mut fonts, Metrics::new(12.0, 13.5));
        text.set_size(Some(58.0), Some(54.0));
        let attrs = Attrs::new().family(Family::Name("Segoe UI"));
        text.set_text(title, &attrs, Shaping::Advanced, None);
        text.shape_until_scroll(&mut fonts, false);
        let white = Color::rgb(240, 240, 240);
        text.draw(&mut fonts, &mut cache, white, |x, y, _, _, color| {
            let (x, y) = (x + 3, y + 8);
            if (0..KEY as i32).contains(&x) && (0..KEY as i32).contains(&y) {
                let pixel = tile.get_pixel_mut(x as u32, y as u32);
                let alpha = u32::from(color.a());
                let source = [color.r(), color.g(), color.b()];
                for (dst, src) in pixel.0.iter_mut().zip(source) {
                    *dst = ((u32::from(src) * alpha + u32::from(*dst) * (255 - alpha)) / 255) as u8;
                }
            }
        });

        let preview = imageops::resize(&tile, 192, 192, imageops::FilterType::Nearest);
        let (x, y) = (16 + i % 3 * 208, 16 + i / 3 * 208);
        imageops::replace(&mut sheet, &preview, x as i64, y as i64);
    }
    sheet.save(&out)?;
    println!("render: {out}");
    Ok(())
}
