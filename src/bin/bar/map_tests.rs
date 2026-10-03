use super::*;

#[test]
fn fits_the_places_padded_and_not_too_close() {
    let p = |lat, lon| unsafe {
        MKMapPoint::for_coordinate(CLLocationCoordinate2D {
            latitude: lat,
            longitude: lon,
        })
    };
    assert!(fit_rect(&[]).is_none());
    let (a, b) = (p(32.0, 34.8), p(31.8, 35.2));
    let r = fit_rect(&[a, b]).unwrap();
    assert!((r.size.width - 2.0 * (b.x - a.x)).abs() < 1e-6); // half again on each side
    assert!((r.origin.x - (a.x - (b.x - a.x) / 2.0)).abs() < 1e-6);
    let one = fit_rect(&[a]).unwrap(); // a single place: MIN_SPAN_M across, centered on it
    let m = unsafe { MKMetersPerMapPointAtLatitude(32.0) };
    assert!((one.size.width * m - MIN_SPAN_M).abs() < 1e-6);
    assert!((one.origin.x + one.size.width / 2.0 - a.x).abs() < 1e-6);
}
