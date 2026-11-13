//! The application icon: the console's mark drawn at the sizes macOS asks for.
//!
//! The console's favicon is a dark rounded square carrying the pepper emoji;
//! the application icon is that same mark with the pepper centered and
//! nothing else, since the Dock has no count to badge it with. It is drawn
//! here with AppKit rather than shipped as a file: the emoji comes from the
//! system's own color font, so the icon matches the console in every release
//! and the appliance carries no image asset.
//!
//! An ICNS holds one PNG per size; every size is rendered at its own pixel
//! count so the Dock and the application switcher pick a crisp image on a
//! Retina display instead of scaling a smaller one.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep,
    NSColor, NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSGraphicsContext,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::WindowError;

/// The mark's glyph, the console's pepper.
const GLYPH: &str = "\u{1F336}\u{FE0F}";
/// The font the console draws the pepper with.
const FONT: &str = "Apple Color Emoji";
/// The background's red, green, and blue, the console favicon's `#101411`.
const BACKGROUND: (f64, f64, f64) = (
    0x10 as f64 / 255.0,
    0x14 as f64 / 255.0,
    0x11 as f64 / 255.0,
);
/// The tile's side as a fraction of the canvas: macOS draws application
/// icons on an 824-point square inside a 1024-point canvas, and a tile that
/// fills the canvas reads larger than every neighbour in the Dock.
const TILE: f64 = 824.0 / 1024.0;
/// Corner radius as a fraction of the tile's side, the favicon's 14 of 64.
const CORNER: f64 = 14.0 / 64.0;
/// The glyph's font size as a fraction of the tile's side.
const GLYPH_SCALE: f64 = 0.62;
/// Each ICNS element type with the pixel size it holds, all PNG-encoded.
const ELEMENTS: [(&[u8; 4], u32); 11] = [
    (b"icp4", 16),
    (b"icp5", 32),
    (b"ic11", 32),
    (b"icp6", 64),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
];

/// The complete ICNS file for the application icon.
pub(crate) fn icns() -> Result<Vec<u8>, WindowError> {
    let mut body = Vec::new();
    let mut rendered: Option<(u32, Vec<u8>)> = None;
    for (kind, side) in ELEMENTS {
        let png = match &rendered {
            Some((size, png)) if *size == side => png.clone(),
            _ => {
                let png = render(side)?;
                rendered = Some((side, png.clone()));
                png
            }
        };
        let length = u32::try_from(png.len() + 8)
            .map_err(|_| WindowError::Platform("the icon image is too large".into()))?;
        body.extend_from_slice(kind);
        body.extend_from_slice(&length.to_be_bytes());
        body.extend_from_slice(&png);
    }
    let total = u32::try_from(body.len() + 8)
        .map_err(|_| WindowError::Platform("the icon is too large".into()))?;
    let mut file = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"icns");
    file.extend_from_slice(&total.to_be_bytes());
    file.extend_from_slice(&body);
    Ok(file)
}

/// One PNG of the mark, `side` pixels square.
fn render(side: u32) -> Result<Vec<u8>, WindowError> {
    let unavailable =
        |what: &str| WindowError::Platform(format!("the icon's {what} is unavailable"));
    let pixels = isize::try_from(side).map_err(|_| unavailable("size"))?;
    // SAFETY: a null plane list asks AppKit to allocate the bitmap itself; the
    // dimensions and sample layout describe ordinary 32-bit RGBA.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            pixels,
            pixels,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .ok_or_else(|| unavailable("bitmap"))?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)
        .ok_or_else(|| unavailable("context"))?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let drawn = draw(f64::from(side));
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    drawn?;
    // SAFETY: an empty property dictionary is a valid, if trivial, set of PNG
    // options.
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or_else(|| unavailable("PNG encoding"))?;
    Ok(png.to_vec())
}

/// Draw the mark into the current context, `side` points square: the tile
/// centered on the canvas with the glyph centered on the tile.
fn draw(side: f64) -> Result<(), WindowError> {
    let (red, green, blue) = BACKGROUND;
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, 1.0).set();
    let tile = side * TILE;
    let inset = (side - tile) / 2.0;
    let radius = tile * CORNER;
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        NSRect::new(NSPoint::new(inset, inset), NSSize::new(tile, tile)),
        radius,
        radius,
    )
    .fill();
    let font = NSFont::fontWithName_size(&NSString::from_str(FONT), tile * GLYPH_SCALE)
        .ok_or_else(|| WindowError::Platform(format!("the {FONT} font is unavailable")))?;
    let font: Retained<AnyObject> = Retained::into_super(Retained::into_super(font));
    // SAFETY: `NSFontAttributeName` is AppKit's constant key for exactly this
    // attribute, and the value under it is a font.
    let glyph = unsafe {
        let attributes = NSDictionary::from_retained_objects(&[NSFontAttributeName], &[font]);
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(GLYPH),
            Some(&attributes),
        )
    };
    let bounds = glyph.size();
    glyph.drawAtPoint(NSPoint::new(
        (side - bounds.width) / 2.0,
        (side - bounds.height) / 2.0,
    ));
    Ok(())
}
