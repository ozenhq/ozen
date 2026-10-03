//! Icons rendered through AppKit: the app's (white ear on an indigo rounded square) and the release DMG's volume
//! icon, the same tile standing on a disk drive with a download badge so the mounted installer doesn't look like
//! the app itself.
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSDeviceRGBColorSpace,
    NSFontWeight, NSFontWeightBold, NSFontWeightMedium, NSGradient, NSGraphicsContext, NSImage,
    NSImageSymbolConfiguration, NSShadow,
};
use objc2_foundation::{NSArray, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Writes `<dir>/AppIcon.icns`, or with `installer` `<dir>/Installer.icns`.
pub fn write(dir: &Path, installer: bool) -> Result<(), String> {
    let name = if installer { "Installer" } else { "AppIcon" };
    let set = std::env::temp_dir().join(format!("ozen-{name}-{}.iconset", std::process::id()));
    let _ = fs::remove_dir_all(&set);
    fs::create_dir_all(&set).map_err(|e| e.to_string())?;
    for size in [16, 32, 128, 256, 512] {
        for (px, suffix) in [(size, ""), (size * 2, "@2x")] {
            let png = unsafe { render(px, installer) };
            fs::write(set.join(format!("icon_{size}x{size}{suffix}.png")), png)
                .map_err(|e| e.to_string())?;
        }
    }
    let out = dir.join(format!("{name}.icns"));
    let ok = Command::new("iconutil")
        .arg("-c")
        .arg("icns")
        .arg(&set)
        .arg("-o")
        .arg(&out)
        .status();
    let _ = fs::remove_dir_all(&set);
    match ok {
        Ok(s) if s.success() => Ok(()),
        r => Err(format!("iconutil: {r:?}")),
    }
}

fn rgb(r: f64, g: f64, b: f64) -> Retained<NSColor> {
    NSColor::colorWithRed_green_blue_alpha(r, g, b, 1.0)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// An SF Symbol in one color, centered on `(cx, cy)`.
unsafe fn symbol(
    name: &str,
    size: f64,
    weight: NSFontWeight,
    color: Retained<NSColor>,
    cx: f64,
    cy: f64,
) {
    let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, weight)
        .configurationByApplyingConfiguration(
            &NSImageSymbolConfiguration::configurationWithPaletteColors(
                &NSArray::from_retained_slice(&[color]),
            ),
        );
    let img = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )
    .and_then(|i| i.imageWithSymbolConfiguration(&cfg));
    if let Some(img) = img {
        let s = img.size();
        img.drawInRect(rect(
            cx - s.width / 2.0,
            cy - s.height / 2.0,
            s.width,
            s.height,
        ));
    }
}

unsafe fn shadow(color: Retained<NSColor>, blur: f64, dy: f64) {
    let s = NSShadow::new();
    s.setShadowColor(Some(&color));
    s.setShadowBlurRadius(blur);
    s.setShadowOffset(NSSize::new(0.0, dy));
    s.set();
}

/// The app tile: indigo rounded square with a white ear.
unsafe fn tile(b: NSRect) {
    unsafe {
        let w = b.size.width;
        let bg = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(b, w * 0.225, w * 0.225);
        NSGradient::initWithStartingColor_endingColor(
            NSGradient::alloc(),
            &rgb(0.36, 0.33, 0.95),
            &rgb(0.18, 0.13, 0.55),
        )
        .expect("gradient")
        .drawInBezierPath_angle(&bg, -90.0);
        let (cx, cy) = (b.origin.x + w / 2.0, b.origin.y + b.size.height / 2.0);
        symbol(
            "ear",
            w * 0.625,
            NSFontWeightMedium,
            NSColor::whiteColor(),
            cx,
            cy,
        );
    }
}

/// A silver external drive across the bottom, with a status light.
unsafe fn drive(s: f64) {
    unsafe {
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect(s * 0.08, s * 0.12, s * 0.84, s * 0.3),
            s * 0.06,
            s * 0.06,
        );
        NSGraphicsContext::saveGraphicsState_class();
        shadow(
            NSColor::blackColor().colorWithAlphaComponent(0.3),
            s * 0.03,
            -s * 0.015,
        );
        NSColor::colorWithWhite_alpha(0.8, 1.0).setFill();
        path.fill();
        NSGraphicsContext::restoreGraphicsState_class();
        NSGradient::initWithStartingColor_endingColor(
            NSGradient::alloc(),
            &NSColor::colorWithWhite_alpha(0.97, 1.0),
            &NSColor::colorWithWhite_alpha(0.76, 1.0),
        )
        .expect("gradient")
        .drawInBezierPath_angle(&path, -90.0);
        NSColor::colorWithWhite_alpha(0.62, 1.0).setStroke();
        path.setLineWidth((s * 0.006).max(1.0));
        path.stroke();
        rgb(0.36, 0.33, 0.95).setFill();
        NSBezierPath::bezierPathWithOvalInRect(rect(s * 0.15, s * 0.19, s * 0.04, s * 0.04)).fill();
    }
}

/// White circle with an indigo down arrow: "this installs something".
unsafe fn badge(r: NSRect) {
    unsafe {
        let w = r.size.width;
        NSGraphicsContext::saveGraphicsState_class();
        shadow(
            rgb(0.18, 0.13, 0.55).colorWithAlphaComponent(0.35),
            w * 0.12,
            -w * 0.04,
        );
        NSColor::whiteColor().setFill();
        NSBezierPath::bezierPathWithOvalInRect(r).fill();
        NSGraphicsContext::restoreGraphicsState_class();
        let (cx, cy) = (r.origin.x + w / 2.0, r.origin.y + r.size.height / 2.0);
        symbol(
            "arrow.down",
            w * 0.55,
            NSFontWeightBold,
            rgb(0.36, 0.33, 0.95),
            cx,
            cy,
        );
    }
}

/// One PNG `px` pixels square.
unsafe fn render(px: usize, installer: bool) -> Vec<u8> {
    unsafe {
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), px as isize, px as isize, 8, 4, true, false,
            NSDeviceRGBColorSpace, 0, 0,
        )
        .expect("bitmap");
        let ctx =
            NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("bitmap context");
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        let s = px as f64;
        if installer {
            drive(s);
            tile(rect(s * 0.24, s * 0.3, s * 0.52, s * 0.52));
            badge(rect(s * 0.6, s * 0.27, s * 0.26, s * 0.26));
        } else {
            tile(rect(s * 0.1, s * 0.1, s * 0.8, s * 0.8));
        }
        NSGraphicsContext::setCurrentContext(None);
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
            .expect("PNG")
            .to_vec()
    }
}

#[cfg(test)]
#[path = "icon_tests.rs"]
mod tests;
