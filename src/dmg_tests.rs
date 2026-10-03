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
