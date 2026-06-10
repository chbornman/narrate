//! The EXIF subset (§9.6): extracted at ingest, stored on `images`,
//! read-only always — the app never writes file metadata.
//!
//! `kamadak-exif` for non-RAW (JPEG/TIFF/PNG/WebP/HEIF containers); rawler
//! metadata for RAW.

use std::io::BufReader;
use std::path::Path;

use exif::{In, Tag, Value};

use super::exclusions::ImageFormat;

/// The §9.6 subset, plus the color hints the preview pipeline consumes
/// (§9.7 — not stored beyond what conversion needs).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ExifSubset {
    pub capture_ts: Option<String>,        // RFC3339 UTC
    pub capture_tz_offset: Option<String>, // '+02:00' etc.
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f64>,
    pub iso: Option<i64>,
    pub f_number: Option<f64>,
    pub exposure_time: Option<String>, // as written, e.g. '1/250'
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub orientation: u16, // 1..8; 1 if absent
    pub pixel_width: Option<u32>,
    pub pixel_height: Option<u32>,
    /// EXIF ColorSpace tag says AdobeRGB (value 2) — §9.7 conversion hint.
    pub adobe_rgb_hint: bool,
}

/// Extract the subset for a file of known format. Failures yield an empty
/// subset with orientation 1 (EXIF is best-effort metadata; the journal
/// never depends on it) except for I/O errors, which propagate so the pass
/// can retry as transient.
pub fn extract(path: &Path, format: ImageFormat) -> std::io::Result<ExifSubset> {
    match format {
        ImageFormat::Raw => Ok(extract_raw(path)),
        _ => extract_container(path, format),
    }
}

// ---------------------------------------------------------------------------
// kamadak-exif path (non-RAW)
// ---------------------------------------------------------------------------

fn extract_container(path: &Path, format: ImageFormat) -> std::io::Result<ExifSubset> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut out = ExifSubset {
        orientation: 1,
        ..Default::default()
    };
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) {
        fill_from_exif(&mut out, &exif);
    }
    // Pixel geometry as stored: the decoder header is authoritative; EXIF
    // PixelXDimension is the fallback (HEIC, which `image` cannot decode).
    if (out.pixel_width.is_none() || matches!(format, ImageFormat::Jpeg | ImageFormat::Png))
        && let Ok((w, h)) = image::image_dimensions(path)
    {
        out.pixel_width = Some(w);
        out.pixel_height = Some(h);
    }
    Ok(out)
}

fn fill_from_exif(out: &mut ExifSubset, exif: &exif::Exif) {
    let field = |tag: Tag| exif.get_field(tag, In::PRIMARY);
    let ascii = |tag: Tag| -> Option<String> {
        field(tag).and_then(|f| match &f.value {
            Value::Ascii(v) => v
                .first()
                .map(|b| String::from_utf8_lossy(b).trim().to_owned())
                .filter(|s| !s.is_empty()),
            _ => None,
        })
    };
    let rational = |tag: Tag| -> Option<(u32, u32)> {
        field(tag).and_then(|f| match &f.value {
            Value::Rational(v) => v.first().map(|r| (r.num, r.denom)),
            _ => None,
        })
    };
    let uint = |tag: Tag| -> Option<u32> { field(tag).and_then(|f| f.value.get_uint(0)) };

    if let Some(o) = uint(Tag::Orientation)
        && (1..=8).contains(&o)
    {
        out.orientation = o as u16;
    }
    out.camera_make = ascii(Tag::Make);
    out.camera_model = ascii(Tag::Model);
    out.lens_model = ascii(Tag::LensModel);
    out.focal_length_mm = rational(Tag::FocalLength)
        .filter(|&(_, d)| d != 0)
        .map(|(n, d)| f64::from(n) / f64::from(d));
    out.iso = uint(Tag::PhotographicSensitivity).map(i64::from);
    out.f_number = rational(Tag::FNumber)
        .filter(|&(_, d)| d != 0)
        .map(|(n, d)| f64::from(n) / f64::from(d));
    out.exposure_time = rational(Tag::ExposureTime).map(|(n, d)| format_exposure(n, d));
    out.pixel_width = uint(Tag::PixelXDimension);
    out.pixel_height = uint(Tag::PixelYDimension);
    out.adobe_rgb_hint = uint(Tag::ColorSpace) == Some(2);

    // GPS
    let gps_coord = |val: Tag, reference: Tag, neg: &str| -> Option<f64> {
        let f = field(val)?;
        let Value::Rational(v) = &f.value else {
            return None;
        };
        if v.len() < 3 || v[0].denom == 0 || v[1].denom == 0 || v[2].denom == 0 {
            return None;
        }
        let deg = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let sign = match ascii(reference) {
            Some(r) if r.eq_ignore_ascii_case(neg) => -1.0,
            _ => 1.0,
        };
        Some(sign * deg)
    };
    out.gps_lat = gps_coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, "S");
    out.gps_lon = gps_coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, "W");

    // Capture time: DateTimeOriginal + OffsetTimeOriginal; fallback
    // DateTimeDigitized (exiftool's CreateDate); else NULL (§9.6).
    let dt = ascii(Tag::DateTimeOriginal)
        .map(|d| (d, ascii(Tag::OffsetTimeOriginal)))
        .or_else(|| ascii(Tag::DateTimeDigitized).map(|d| (d, ascii(Tag::OffsetTimeDigitized))));
    if let Some((dt, offset)) = dt
        && let Some((ts, tz)) = capture_ts_utc(&dt, offset.as_deref())
    {
        out.capture_ts = Some(ts);
        out.capture_tz_offset = tz;
    }
}

// ---------------------------------------------------------------------------
// rawler path (RAW)
// ---------------------------------------------------------------------------

fn extract_raw(path: &Path) -> ExifSubset {
    let mut out = ExifSubset {
        orientation: 1,
        ..Default::default()
    };
    let Ok(source) = rawler::rawsource::RawSource::new(path) else {
        return out;
    };
    let Ok(decoder) = rawler::get_decoder(&source) else {
        return out;
    };
    let params = rawler::decoders::RawDecodeParams::default();
    if let Ok(md) = decoder.raw_metadata(&source, &params) {
        let e = &md.exif;
        if let Some(o) = e.orientation
            && (1..=8).contains(&o)
        {
            out.orientation = o;
        }
        out.camera_make = none_if_empty(md.make.clone());
        out.camera_model = none_if_empty(md.model.clone());
        out.lens_model = e.lens_model.clone();
        out.focal_length_mm = e
            .focal_length
            .filter(|r| r.d != 0)
            .map(|r| f64::from(r.n) / f64::from(r.d));
        out.iso = e
            .iso_speed_ratings
            .map(i64::from)
            .or(e.iso_speed.map(i64::from));
        out.f_number = e
            .fnumber
            .filter(|r| r.d != 0)
            .map(|r| f64::from(r.n) / f64::from(r.d));
        out.exposure_time = e.exposure_time.map(|r| format_exposure(r.n, r.d));
        out.adobe_rgb_hint = e.color_space == Some(2);
        if let Some(gps) = &e.gps {
            let coord = |triple: &Option<[rawler::formats::tiff::Rational; 3]>,
                         reference: &Option<String>,
                         neg: &str| {
                triple.as_ref().and_then(|v| {
                    if v.iter().any(|r| r.d == 0) {
                        return None;
                    }
                    let deg = f64::from(v[0].n) / f64::from(v[0].d)
                        + f64::from(v[1].n) / f64::from(v[1].d) / 60.0
                        + f64::from(v[2].n) / f64::from(v[2].d) / 3600.0;
                    let sign = match reference {
                        Some(r) if r.eq_ignore_ascii_case(neg) => -1.0,
                        _ => 1.0,
                    };
                    Some(sign * deg)
                })
            };
            out.gps_lat = coord(&gps.gps_latitude, &gps.gps_latitude_ref, "S");
            out.gps_lon = coord(&gps.gps_longitude, &gps.gps_longitude_ref, "W");
        }
        let dt = e
            .date_time_original
            .clone()
            .map(|d| (d, e.offset_time_original.clone()))
            .or_else(|| {
                e.create_date
                    .clone()
                    .map(|d| (d, e.offset_time_digitized.clone()))
            });
        if let Some((dt, offset)) = dt
            && let Some((ts, tz)) = capture_ts_utc(&dt, offset.as_deref())
        {
            out.capture_ts = Some(ts);
            out.capture_tz_offset = tz;
        }
    }
    out
}

fn none_if_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

/// Exposure as written (§6): '1/250', '30', '5/2'.
fn format_exposure(n: u32, d: u32) -> String {
    if d == 1 || d == 0 {
        format!("{n}")
    } else {
        format!("{n}/{d}")
    }
}

/// Convert EXIF "YYYY:MM:DD HH:MM:SS" plus an optional '+HH:MM' offset to
/// (RFC3339 UTC, kept offset). Without an offset the wall time is stored
/// as-is with a `Z` and `capture_tz_offset` NULL — flagged reading: EXIF
/// gives no zone, and inventing one would be worse than honest ambiguity.
pub fn capture_ts_utc(datetime: &str, offset: Option<&str>) -> Option<(String, Option<String>)> {
    let dt = datetime.trim();
    let b = dt.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |s: &str| -> Option<i64> { s.parse().ok() };
    let y = num(dt.get(0..4)?)?;
    let mo = num(dt.get(5..7)?)?;
    let d = num(dt.get(8..10)?)?;
    let h = num(dt.get(11..13)?)?;
    let mi = num(dt.get(14..16)?)?;
    let s = num(dt.get(17..19)?)?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    let mut epoch_ms = (days * 86_400 + h * 3_600 + mi * 60 + s) * 1_000;
    let mut kept_offset = None;
    if let Some(off) = offset
        && let Some(off_min) = parse_offset_minutes(off)
    {
        epoch_ms -= off_min * 60_000;
        kept_offset = Some(off.trim().to_owned());
    }
    Some((
        crate::id::UtcMillis::from_epoch_ms(epoch_ms).to_rfc3339(),
        kept_offset,
    ))
}

/// '+02:00' / '-05:30' → signed minutes.
fn parse_offset_minutes(s: &str) -> Option<i64> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (h, m) = rest.split_once(':')?;
    let h: i64 = h.parse().ok()?;
    let m: i64 = m.parse().ok()?;
    if h > 14 || m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm; duplicated privately —
/// `id.rs` is outside this packet's ownership).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_ts_with_offset_converts_to_utc() {
        let (ts, tz) = capture_ts_utc("2026:06:09 14:23:05", Some("+02:00")).unwrap();
        assert_eq!(ts, "2026-06-09T12:23:05.000Z");
        assert_eq!(tz.as_deref(), Some("+02:00"));
    }

    #[test]
    fn capture_ts_without_offset_keeps_wall_time() {
        let (ts, tz) = capture_ts_utc("2026:06:09 14:23:05", None).unwrap();
        assert_eq!(ts, "2026-06-09T14:23:05.000Z");
        assert_eq!(tz, None);
    }

    #[test]
    fn exposure_formatting() {
        assert_eq!(format_exposure(1, 250), "1/250");
        assert_eq!(format_exposure(30, 1), "30");
        assert_eq!(format_exposure(5, 2), "5/2");
    }

    #[test]
    fn invalid_datetime_is_none() {
        assert!(capture_ts_utc("garbage", None).is_none());
        assert!(capture_ts_utc("2026:13:09 14:23:05", None).is_none());
    }
}
