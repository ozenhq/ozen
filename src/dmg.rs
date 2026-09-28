//! `ozen dmg <Ozen.app> <out.dmg>`: the release DMG (.github/workflows/release.yml). Ozen.app and an Applications
//! link on a drawn background, laid out in the volume's .DS_Store so Finder opens it as an install window.
use dmg_layout::{
    AliasHeader, AliasKind, AliasRecord, AliasTarget, AliasVolume, CatalogNodeId, DiskType,
    ExtraEntry, ExtrasTag, FilesystemSignature, HfsTimestampSeconds, VolumeAttributes,
};
use ds_parser::{
    BrowserWindowSettings, FieldKey, FourCC, IconLocation, IconViewProperties, Record, Value,
    WindowBounds,
};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSBezierPath, NSBitmapImageRep, NSColor,
    NSDeviceRGBColorSpace, NSFont, NSFontAttributeName, NSFontWeight, NSFontWeightBold,
    NSFontWeightMedium, NSFontWeightRegular, NSForegroundColorAttributeName, NSGradient,
    NSGraphicsContext, NSImage, NSKernAttributeName, NSLineCapStyle, NSLineJoinStyle,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSShadow, NSTextAlignment,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString,
};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::time::UNIX_EPOCH;

const W: f64 = 640.0; // background size in points; the window is 30pt taller for its title bar
const H: f64 = 400.0;
const ICONS: [(&str, f64); 2] = [("Ozen.app", 170.0), ("Applications", 470.0)]; // centers, 210pt from the top
const ICON_Y: f64 = 210.0;
const BG: &str = ".background.tiff";
const VOLUME: &[u8] = b"Ozen";

pub fn build(app: &str, out: &str) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("ozen-dmg-{}", std::process::id()));
    let (stage, mnt, rw) = (tmp.join("stage"), tmp.join("mnt"), tmp.join("rw.dmg"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    run("ditto", &[app, &stage.join("Ozen.app").to_string_lossy()])?;
    std::os::unix::fs::symlink("/Applications", stage.join("Applications"))
        .map_err(|e| e.to_string())?;
    fs::write(stage.join(BG), background()).map_err(|e| e.to_string())?;
    // the installer icon (a drive), so the mounted DMG doesn't look like the app
    crate::icon::write(&tmp, true)?;
    fs::rename(tmp.join("Installer.icns"), stage.join(".VolumeIcon.icns"))
        .map_err(|e| e.to_string())?;
    let (s, m, r) = (
        stage.to_string_lossy(),
        mnt.to_string_lossy(),
        rw.to_string_lossy(),
    );
    run(
        "hdiutil",
        &[
            "create",
            "-quiet",
            "-srcfolder",
            &s,
            "-volname",
            "Ozen",
            "-fs",
            "HFS+",
            "-format",
            "UDRW",
            &r,
        ],
    )?;
    run(
        "hdiutil",
        &[
            "attach",
            "-quiet",
            "-nobrowse",
            "-noautoopen",
            "-readwrite",
            "-mountpoint",
            &m,
            &r,
        ],
    )?;
    // The .DS_Store points at the background by the mounted volume's own dates and file ids, so write it there.
    let laid =
        layout(&mnt).and_then(|ds| fs::write(mnt.join(".DS_Store"), ds).map_err(|e| e.to_string()));
    // FinderInfo "has custom icon" flag on the volume root: Finder then shows .VolumeIcon.icns
    let flagged = run(
        "xattr",
        &[
            "-wx",
            "com.apple.FinderInfo",
            &format!("{:0<64}", "00000000000000000400"),
            &m,
        ],
    );
    run("hdiutil", &["detach", "-quiet", &m])?;
    laid.and(flagged)?;
    let _ = fs::remove_file(out);
    run(
        "hdiutil",
        &[
            "convert",
            "-quiet",
            &r,
            "-format",
            "UDZO",
            "-imagekey",
            "zlib-level=9",
            "-o",
            out,
        ],
    )?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    match Command::new(cmd).args(args).status() {
        Ok(s) if s.success() => Ok(()),
        r => Err(format!("{cmd} {args:?}: {r:?}")),
    }
}

/// The records dmgbuild wrote for the same layout: window chrome off, icon view on the background, icon spots.
fn layout(vol: &Path) -> Result<Vec<u8>, String> {
    let bounds = WindowBounds {
        x: 200.0,
        y: 120.0,
        width: W,
        height: H + 30.0,
    };
    let mut window = BrowserWindowSettings::builder()
        .window_bounds(bounds)
        .show_sidebar(false)
        .show_toolbar(false)
        .show_status_bar(false)
        .show_path_bar(false)
        .show_tab_view(false)
        .container_show_sidebar(false)
        .sidebar_width(180.0)
        .build();
    window
        .unknown
        .insert("PreviewPaneVisibility".into(), plist::Value::Boolean(false));
    let mut view = IconViewProperties {
        arrange_by: Some("none".into()),
        background_type: Some(2), // picture
        background_color: Some((1.0, 1.0, 1.0)),
        grid_offset: Some((0.0, 0.0)),
        grid_spacing: Some(100.0),
        icon_size: Some(112.0),
        text_size: Some(13.0),
        label_on_bottom: Some(true),
        show_icon_preview: Some(false),
        show_item_info: Some(false),
        view_options_version: Some(1),
        ..Default::default()
    };
    view.unknown.insert(
        "backgroundImageAlias".into(),
        plist::Value::Data(alias(vol, BG)?),
    );
    for k in ["scrollPositionX", "scrollPositionY"] {
        view.unknown.insert(k.into(), plist::Value::Real(0.0));
    }
    let mut records = vec![
        Record::new(".", FieldKey::bwsp, Value::BrowserWindowSettings(window)),
        Record::new(
            ".",
            FieldKey::icvl,
            Value::Type(FourCC(b'i', b'c', b'n', b'v')),
        ),
        Record::new(".", FieldKey::icvp, Value::IconViewProperties(view)),
        Record::new(".", FieldKey::vSrn, Value::Long(1)), // store version; Finder ignores the view settings without it
    ];
    for (name, x) in ICONS {
        records.push(Record::new(
            name,
            FieldKey::Iloc,
            IconLocation::new(x as u32, ICON_Y as u32),
        ));
    }
    ds_parser::write(&records).map_err(|e| e.to_string())
}

/// A classic alias to `name` at the volume root, which is what Finder resolves `backgroundImageAlias` through;
/// the same fields and extras dmgbuild writes.
fn alias(vol: &Path, name: &str) -> Result<Vec<u8>, String> {
    let hfs = |p: &Path| -> Result<u32, String> {
        let t = fs::metadata(p)
            .and_then(|m| m.created())
            .map_err(|e| e.to_string())?;
        let unix = t
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        Ok((unix + 2_082_844_800) as u32) // HFS counts from 1904
    };
    let hi_res = |secs: u32| ((secs as u64) << 16).to_be_bytes().to_vec(); // 48.16 fixed point
    let file = vol.join(name);
    let id = fs::metadata(&file).map_err(|e| e.to_string())?.ino() as u32; // an HFS+ inode number is its CNID
    let (vol_date, file_date) = (hfs(vol)?, hfs(&file)?);
    let e = |r: Result<ExtraEntry, _>| r.map_err(|e: dmg_layout::DiskImageError| e.to_string());
    let volume = AliasVolume::new(
        VOLUME,
        HfsTimestampSeconds(vol_date),
        FilesystemSignature::HIERARCHICAL_FILE_SYSTEM_PLUS,
        DiskType::Fixed,
    )
    .map_err(|e| e.to_string())?;
    let target = AliasTarget::new(
        AliasKind::File,
        CatalogNodeId(2),
        name,
        CatalogNodeId(id),
        HfsTimestampSeconds(file_date),
    )
    .map_err(|e| e.to_string())?;
    let mut header = AliasHeader::new(volume, target);
    header.volume_attributes = VolumeAttributes(0);
    let extras = vec![
        e(ExtraEntry::carbon_folder_name(VOLUME))?,
        e(ExtraEntry::new(
            ExtrasTag::HighResolutionVolumeCreationDate,
            hi_res(vol_date),
        ))?,
        e(ExtraEntry::new(
            ExtrasTag::HighResolutionCreationDate,
            hi_res(file_date),
        ))?,
        e(ExtraEntry::new(
            ExtrasTag::CarbonPath,
            format!("Ozen:{name}").into_bytes(),
        ))?,
        e(ExtraEntry::unicode_target_name(name))?,
        e(ExtraEntry::unicode_volume_name("Ozen"))?,
        e(ExtraEntry::portable_operating_system_interface_path(
            format!("/{name}").into_bytes(),
        ))?,
        e(ExtraEntry::portable_operating_system_interface_volume_path(
            b"/Volumes/Ozen",
        ))?,
    ];
    AliasRecord::new(header, extras)
        .and_then(|a| a.to_bytes())
        .map_err(|e| e.to_string())
}

/// The window background at 1x and 2x in one TIFF, in the app icon's indigo: title, frosted cards behind the two
/// icons, a dashed arrow between them, and the first-open hint.
fn background() -> Vec<u8> {
    unsafe {
        let img = NSImage::initWithSize(NSImage::alloc(), NSSize::new(W, H));
        for scale in [1.0, 2.0] {
            img.addRepresentation(&render(scale));
        }
        img.TIFFRepresentation().expect("background TIFF").to_vec()
    }
}

fn rgb(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
    NSColor::colorWithRed_green_blue_alpha(r, g, b, a)
}

unsafe fn text(s: &str, size: f64, weight: NSFontWeight, color: &NSColor, y: f64) {
    unsafe {
        let p = NSMutableParagraphStyle::new();
        p.setAlignment(NSTextAlignment::Center);
        let font = NSFont::systemFontOfSize_weight(size, weight);
        let kern = NSNumber::new_f64(if size > 30.0 { -0.5 } else { 0.0 });
        let attrs = NSDictionary::from_slices(
            &[
                NSFontAttributeName,
                NSForegroundColorAttributeName,
                NSParagraphStyleAttributeName,
                NSKernAttributeName,
            ],
            &[font.as_ref(), color.as_ref(), p.as_ref(), kern.as_ref()],
        );
        let s = NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(s),
            Some(&attrs),
        );
        s.drawInRect(NSRect::new(
            NSPoint::new(0.0, y),
            NSSize::new(W, size * 1.4),
        ));
    }
}

unsafe fn render(scale: f64) -> Retained<NSBitmapImageRep> {
    unsafe {
        let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), (W * scale) as isize, (H * scale) as isize, 8, 4, true,
            false, NSDeviceRGBColorSpace, 0, 0,
        )
        .expect("bitmap");
        rep.setSize(NSSize::new(W, H)); // draw in points; the rep holds scale x pixels
        let ctx =
            NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("bitmap context");
        NSGraphicsContext::setCurrentContext(Some(&ctx));
        let indigo = rgb(0.36, 0.33, 0.95, 1.0);
        let deep = rgb(0.18, 0.13, 0.55, 1.0);
        let all = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H));
        NSGradient::initWithStartingColor_endingColor(
            NSGradient::alloc(),
            &rgb(0.97, 0.97, 1.0, 1.0),
            &rgb(0.90, 0.89, 0.99, 1.0),
        )
        .expect("gradient")
        .drawInRect_angle(all, -90.0);
        let y = H - ICON_Y; // Finder measures from the top, AppKit from the bottom
        for (_, x) in ICONS {
            NSGraphicsContext::saveGraphicsState_class();
            let shadow = NSShadow::new();
            shadow.setShadowColor(Some(&deep.colorWithAlphaComponent(0.12)));
            shadow.setShadowBlurRadius(18.0);
            shadow.setShadowOffset(NSSize::new(0.0, -4.0));
            shadow.set();
            rgb(1.0, 1.0, 1.0, 0.7).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                NSRect::new(NSPoint::new(x - 85.0, y - 98.0), NSSize::new(170.0, 184.0)),
                26.0,
                26.0,
            )
            .fill();
            NSGraphicsContext::restoreGraphicsState_class();
        }
        text("Ozen", 34.0, NSFontWeightBold, &deep, H - 78.0);
        text(
            "Drag Ozen into Applications to install",
            14.0,
            NSFontWeightRegular,
            &deep.colorWithAlphaComponent(0.65),
            H - 104.0,
        );
        indigo.setStroke();
        let line = NSBezierPath::new();
        line.moveToPoint(NSPoint::new(250.0, y));
        line.lineToPoint(NSPoint::new(380.0, y));
        line.setLineWidth(3.0);
        line.setLineCapStyle(NSLineCapStyle::Round);
        let dash = [2.0, 9.0];
        line.setLineDash_count_phase(dash.as_ptr(), 2, 0.0);
        line.stroke();
        let head = NSBezierPath::new();
        head.moveToPoint(NSPoint::new(378.0, y + 12.0));
        head.lineToPoint(NSPoint::new(392.0, y));
        head.lineToPoint(NSPoint::new(378.0, y - 12.0));
        head.setLineWidth(3.0);
        head.setLineCapStyle(NSLineCapStyle::Round);
        head.setLineJoinStyle(NSLineJoinStyle::Round);
        head.stroke();
        // above the status bar Finder may show
        text(
            "First open: System Settings › Privacy & Security › Open Anyway",
            11.0,
            NSFontWeightMedium,
            &deep.colorWithAlphaComponent(0.5),
            46.0,
        );
        NSGraphicsContext::setCurrentContext(None);
        rep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_places_icons_and_points_at_the_background() {
        let vol = std::env::temp_dir().join(format!("ozen-dmg-test-{}", std::process::id()));
        fs::create_dir_all(&vol).unwrap();
        fs::write(vol.join(BG), b"tiff").unwrap();
        let ino = fs::metadata(vol.join(BG)).unwrap().ino() as u32;
        let store = ds_parser::parse(&layout(&vol).unwrap()).unwrap();
        let _ = fs::remove_dir_all(&vol);
        let find = |name: &str, field| {
            store
                .records
                .iter()
                .find(|r| r.name == name && r.field == field)
                .map(|r| &r.value)
        };
        for (name, x) in ICONS {
            let Some(Value::IconLocation(at)) = find(name, FieldKey::Iloc) else {
                panic!("no Iloc for {name}")
            };
            assert_eq!((at.x, at.y), (x as u32, ICON_Y as u32));
        }
        assert_eq!(find(".", FieldKey::vSrn), Some(&Value::Long(1)));
        let Some(Value::IconViewProperties(view)) = find(".", FieldKey::icvp) else {
            panic!("no icvp")
        };
        let Some(plist::Value::Data(a)) = view.unknown.get("backgroundImageAlias") else {
            panic!("no alias")
        };
        let a = AliasRecord::from_bytes(a).unwrap();
        assert_eq!(a.header.target_name, BG.as_bytes());
        assert_eq!(a.header.target_catalog_node_id, CatalogNodeId(ino));
    }
}
