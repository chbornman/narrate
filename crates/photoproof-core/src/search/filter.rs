//! Filter execution (RETRIEVAL §4/§5.1): chips compile to hard WHERE
//! constraints — never ranking inputs. Image-scoped filters apply per joined
//! target; event-scoped filters (annotation date, source) apply to the
//! matched root event.

use rusqlite::types::Value;

use crate::id::UtcMillis;

use super::{
    Comparison, DateField, DateRange, Filter, PathMatch, RelativeRange, SearchError, Season,
    StringMatch, VolumeFilter,
};

/// Which statement the filters are being compiled into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterMode {
    /// §4 image-hit statement: per joined target (`t.image_hash`), root
    /// event joinable as `e`, images joinable as `i`.
    Hits,
    /// Filter-only browse over `images i`.
    Browse,
}

pub(crate) struct CompiledFilters {
    /// Statement needs `JOIN annotation_events e ON e.id = m.root_event_id`.
    pub join_event: bool,
    /// Statement needs `JOIN images i ON i.image_hash = t.image_hash`.
    pub join_images: bool,
    pub wheres: Vec<String>,
    pub params: Vec<Value>,
    /// Any image-scoped filter present: session-level hits cannot satisfy
    /// an image constraint, so the statement inner-joins `event_targets`
    /// and yields no session hits (R4).
    pub image_scoped: bool,
    /// A `HasStrokes(true)` chip is present: filter-only browse results
    /// carry `Provenance::Stroke` (matched via stroke evidence, §5.4).
    pub stroke_evidence: bool,
}

/// The image-hash expression each mode filters against.
fn img(mode: FilterMode) -> &'static str {
    match mode {
        FilterMode::Hits => "t.image_hash",
        FilterMode::Browse => "i.image_hash",
    }
}

pub(crate) fn compile(
    filters: &[Filter],
    mode: FilterMode,
    now: UtcMillis,
) -> Result<CompiledFilters, SearchError> {
    let mut cf = CompiledFilters {
        join_event: false,
        join_images: false,
        wheres: Vec::new(),
        params: Vec::new(),
        image_scoped: false,
        stroke_evidence: false,
    };
    for f in filters {
        match f {
            Filter::Date { field, range } => {
                let (start, end) = resolve_range(range, now);
                match field {
                    DateField::Captured => {
                        // NULL capture_ts never satisfies a bound: undated
                        // images do not match a captured-date chip.
                        cf.image_scoped = true;
                        image_column_bounds(&mut cf, mode, "capture_ts", start, end);
                    }
                    DateField::Annotated => match mode {
                        FilterMode::Hits => {
                            // The matched root event's ts (§7.1: the filter
                            // applies to the events, as WHERE on the join).
                            cf.join_event = true;
                            push_bounds(&mut cf, "e.ts", start, end);
                        }
                        FilterMode::Browse => {
                            // Any live content event in range targeting the
                            // image (liveness per B4: retraction folds out;
                            // scrubbed stubs still count).
                            let mut inner = String::new();
                            if let Some(s) = start {
                                inner.push_str(" AND be.ts >= ?");
                                cf.params.push(Value::Text(s.to_rfc3339()));
                            }
                            if let Some(e) = end {
                                inner.push_str(" AND be.ts < ?");
                                cf.params.push(Value::Text(e.to_rfc3339()));
                            }
                            cf.wheres.push(live_event_exists(mode, &inner));
                        }
                    },
                }
            }
            Filter::Camera(m) => {
                cf.image_scoped = true;
                image_string_match(&mut cf, mode, "camera_model", m);
            }
            Filter::Lens(m) => {
                cf.image_scoped = true;
                image_string_match(&mut cf, mode, "lens_model", m);
            }
            Filter::Folder(pm) => {
                cf.image_scoped = true;
                match pm {
                    PathMatch::Subtree(p) => {
                        let p = p.trim_matches('/');
                        if p.is_empty() {
                            cf.wheres.push(format!(
                                "EXISTS (SELECT 1 FROM paths sp WHERE sp.image_hash = {} \
                                 AND sp.state = 'active')",
                                img(mode)
                            ));
                        } else {
                            cf.wheres.push(format!(
                                "EXISTS (SELECT 1 FROM paths sp WHERE sp.image_hash = {} \
                                 AND sp.state = 'active' \
                                 AND sp.rel_path LIKE ? ESCAPE '\\')",
                                img(mode)
                            ));
                            cf.params.push(Value::Text(format!("{}/%", like_escape(p))));
                        }
                    }
                    PathMatch::NameContains(s) => {
                        // The substring must land in the directory portion of
                        // an active path: it is followed (not necessarily
                        // immediately) by a `/` — filename matches never
                        // qualify as a folder-name match.
                        cf.wheres.push(format!(
                            "EXISTS (SELECT 1 FROM paths sp WHERE sp.image_hash = {} \
                             AND sp.state = 'active' \
                             AND sp.rel_path LIKE ? ESCAPE '\\')",
                            img(mode)
                        ));
                        cf.params
                            .push(Value::Text(format!("%{}%/%", like_escape(s))));
                    }
                }
            }
            Filter::Root(name) => {
                cf.image_scoped = true;
                cf.wheres.push(format!(
                    "EXISTS (SELECT 1 FROM paths sp \
                     JOIN roots sr ON sr.root_id = sp.root_id \
                     WHERE sp.image_hash = {} AND sp.state = 'active' \
                     AND sr.state = 'active' \
                     AND sr.display_name = ? COLLATE NOCASE)",
                    img(mode)
                ));
                cf.params.push(Value::Text(name.clone()));
            }
            Filter::Rating(cmp) => {
                // E4/B4: reads the `image_ratings` fold. An unrated image
                // has no row and matches no rating filter; `value:0` is an
                // explicit zero and matches `Eq(0)`/`Lte(n)`.
                cf.image_scoped = true;
                let cond = match cmp {
                    Comparison::Eq(v) => {
                        cf.params.push(Value::Integer(i64::from(*v)));
                        "srt.rating = ?".to_string()
                    }
                    Comparison::Gte(v) => {
                        cf.params.push(Value::Integer(i64::from(*v)));
                        "srt.rating >= ?".to_string()
                    }
                    Comparison::Lte(v) => {
                        cf.params.push(Value::Integer(i64::from(*v)));
                        "srt.rating <= ?".to_string()
                    }
                    Comparison::Between(a, b) => {
                        let (lo, hi) = (*a.min(b), *a.max(b));
                        cf.params.push(Value::Integer(i64::from(lo)));
                        cf.params.push(Value::Integer(i64::from(hi)));
                        "srt.rating BETWEEN ? AND ?".to_string()
                    }
                };
                cf.wheres.push(format!(
                    "EXISTS (SELECT 1 FROM image_ratings srt \
                     WHERE srt.image_hash = {} AND {})",
                    img(mode),
                    cond
                ));
            }
            Filter::HasStrokes(want) => {
                // §4/P5: an indexed WHERE on `image_journal_stats.has_strokes`
                // — never a stroke-event fold at query time.
                cf.image_scoped = true;
                if *want {
                    cf.stroke_evidence = true;
                }
                let prefix = if *want { "" } else { "NOT " };
                cf.wheres.push(format!(
                    "{}EXISTS (SELECT 1 FROM image_journal_stats sst \
                     WHERE sst.image_hash = {} AND sst.has_strokes = 1)",
                    prefix,
                    img(mode)
                ));
            }
            Filter::Source(sources) => {
                if sources.is_empty() {
                    // An empty hard constraint admits nothing.
                    cf.wheres.push("0".into());
                    continue;
                }
                let marks = vec!["?"; sources.len()].join(", ");
                match mode {
                    FilterMode::Hits => {
                        cf.join_event = true;
                        cf.wheres.push(format!("e.source IN ({marks})"));
                    }
                    FilterMode::Browse => {
                        let inner = format!(" AND be.source IN ({marks})");
                        cf.wheres.push(live_event_exists(mode, &inner));
                    }
                }
                for s in sources {
                    cf.params.push(Value::Text(s.as_str().to_owned()));
                }
            }
            Filter::Volume(vf) => {
                cf.image_scoped = true;
                match vf {
                    VolumeFilter::Online => cf.wheres.push(online_exists(mode, "")),
                    // Offline = not available (LIBRARY §8: available iff an
                    // active path sits on an online volume).
                    VolumeFilter::Offline => {
                        cf.wheres.push(format!("NOT {}", online_exists(mode, "")))
                    }
                    VolumeFilter::Named(label) => {
                        cf.wheres.push(format!(
                            "EXISTS (SELECT 1 FROM paths sp \
                             JOIN volumes sv ON sv.volume_id = sp.volume_id \
                             WHERE sp.image_hash = {} AND sp.state = 'active' \
                             AND sv.label = ? COLLATE NOCASE)",
                            img(mode)
                        ));
                        cf.params.push(Value::Text(label.clone()));
                    }
                }
            }
            Filter::Project(_) => return Err(SearchError::UnsupportedFilter("project")),
            Filter::Kind(_) => return Err(SearchError::UnsupportedFilter("kind")),
        }
    }
    Ok(cf)
}

/// `EXISTS` an active path on an online volume.
fn online_exists(mode: FilterMode, extra: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM paths sp \
         JOIN volumes sv ON sv.volume_id = sp.volume_id \
         WHERE sp.image_hash = {} AND sp.state = 'active' \
         AND sv.state = 'online'{extra})",
        img(mode)
    )
}

/// `EXISTS` a live (non-retracted) content event targeting the image, with
/// extra per-event conditions on alias `be` (browse-mode event filters).
fn live_event_exists(mode: FilterMode, extra: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM event_targets bt \
         JOIN annotation_events be ON be.id = bt.event_id \
         WHERE bt.image_hash = {} \
         AND be.kind IN ('remark', 'rating', 'stroke') \
         AND NOT EXISTS (SELECT 1 FROM annotation_events br \
                         WHERE br.target_event = be.id AND br.kind = 'retraction')\
         {extra})",
        img(mode)
    )
}

/// Camera/lens string match against an `images` column. Case-insensitivity
/// is SQLite's NOCASE/LIKE (ASCII) — EXIF gear strings are ASCII in practice.
fn image_string_match(cf: &mut CompiledFilters, mode: FilterMode, col: &str, m: &StringMatch) {
    if mode == FilterMode::Hits {
        cf.join_images = true;
    }
    match m {
        StringMatch::Exact(v) => {
            cf.wheres.push(format!("i.{col} = ? COLLATE NOCASE"));
            cf.params.push(Value::Text(v.clone()));
        }
        StringMatch::Contains(v) => {
            cf.wheres.push(format!("i.{col} LIKE ? ESCAPE '\\'"));
            cf.params.push(Value::Text(format!("%{}%", like_escape(v))));
        }
    }
}

/// Half-open bounds on an `images` column.
fn image_column_bounds(
    cf: &mut CompiledFilters,
    mode: FilterMode,
    col: &str,
    start: Option<UtcMillis>,
    end: Option<UtcMillis>,
) {
    if mode == FilterMode::Hits {
        cf.join_images = true;
    }
    push_bounds(cf, &format!("i.{col}"), start, end);
}

fn push_bounds(
    cf: &mut CompiledFilters,
    expr: &str,
    start: Option<UtcMillis>,
    end: Option<UtcMillis>,
) {
    if let Some(s) = start {
        cf.wheres.push(format!("{expr} >= ?"));
        cf.params.push(Value::Text(s.to_rfc3339()));
    }
    if let Some(e) = end {
        cf.wheres.push(format!("{expr} < ?"));
        cf.params.push(Value::Text(e.to_rfc3339()));
    }
}

/// Escape `%`, `_`, and the escape character itself for `LIKE … ESCAPE '\'`.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// §5.1 date-range resolution (relative ranges anchor on `now`)
// ---------------------------------------------------------------------------

/// Resolve a range to half-open `[start, end)` bounds.
pub(crate) fn resolve_range(
    range: &DateRange,
    now: UtcMillis,
) -> (Option<UtcMillis>, Option<UtcMillis>) {
    match range {
        DateRange::Absolute { start, end } => (*start, *end),
        DateRange::Relative(rel) => {
            let (y, m, d, tail) = split_ts(now);
            match rel {
                RelativeRange::LastDays(n) => (Some(days_back(now, i64::from(*n))), None),
                RelativeRange::LastWeeks(n) => (Some(days_back(now, i64::from(*n) * 7)), None),
                RelativeRange::LastMonths(n) => {
                    let (yy, mm) = add_months(y, m, -i64::from(*n));
                    (Some(make_ts(yy, mm, d, &tail)), None)
                }
                RelativeRange::LastYears(n) => (Some(make_ts(y - *n as i32, m, d, &tail)), None),
                RelativeRange::Season { season, years_ago } => {
                    // §7.1 normative anchor: run June 2026, "last winter"
                    // (years_ago: 1) = [2025-12-01, 2026-03-01). The season
                    // occurrence is keyed by the calendar year containing
                    // its start: start_year = year(now) − years_ago.
                    let sy = y - *years_ago as i32;
                    let (sm, ey, em) = match season {
                        Season::Spring => (3, sy, 6),
                        Season::Summer => (6, sy, 9),
                        Season::Autumn => (9, sy, 12),
                        Season::Winter => (12, sy + 1, 3),
                    };
                    (Some(month_floor(sy, sm)), Some(month_floor(ey, em)))
                }
                RelativeRange::Month { month, year } => {
                    let mm = u32::from((*month).clamp(1, 12));
                    let yy = year.unwrap_or(if mm > m { y - 1 } else { y });
                    let (ny, nm) = add_months(yy, mm, 1);
                    (Some(month_floor(yy, mm)), Some(month_floor(ny, nm)))
                }
                RelativeRange::Year(yy) => {
                    (Some(month_floor(*yy, 1)), Some(month_floor(yy + 1, 1)))
                }
            }
        }
    }
}

fn days_back(now: UtcMillis, days: i64) -> UtcMillis {
    UtcMillis::from_epoch_ms(now.epoch_ms() - days * 86_400_000)
}

/// (year, month, day, time-of-day tail `Thh:mm:ss.mmmZ`) of a timestamp,
/// via its canonical RFC 3339 form.
fn split_ts(ts: UtcMillis) -> (i32, u32, u32, String) {
    let s = ts.to_rfc3339();
    let y: i32 = s[0..4].parse().expect("canonical rfc3339");
    let m: u32 = s[5..7].parse().expect("canonical rfc3339");
    let d: u32 = s[8..10].parse().expect("canonical rfc3339");
    (y, m, d, s[10..].to_owned())
}

/// Build a timestamp from civil parts, clamping the day to the month's
/// length (e.g. May 31 − 3 months → Feb 28/29).
fn make_ts(y: i32, m: u32, d: u32, tail: &str) -> UtcMillis {
    let d = d.min(days_in_month(y, m));
    UtcMillis::parse(&format!("{y:04}-{m:02}-{d:02}{tail}")).expect("constructed canonical form")
}

fn month_floor(y: i32, m: u32) -> UtcMillis {
    UtcMillis::parse(&format!("{y:04}-{m:02}-01T00:00:00.000Z"))
        .expect("constructed canonical form")
}

fn add_months(y: i32, m: u32, delta: i64) -> (i32, u32) {
    let total = i64::from(y) * 12 + i64::from(m) - 1 + delta;
    let yy = total.div_euclid(12);
    let mm = total.rem_euclid(12) + 1;
    (yy as i32, mm as u32)
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> UtcMillis {
        UtcMillis::parse(s).unwrap()
    }

    #[test]
    fn last_winter_resolves_per_spec_walkthrough() {
        // §7.1: run June 2026 → [2025-12-01, 2026-03-01).
        let now = ts("2026-06-15T12:00:00.000Z");
        let (start, end) = resolve_range(
            &DateRange::Relative(RelativeRange::Season {
                season: Season::Winter,
                years_ago: 1,
            }),
            now,
        );
        assert_eq!(start.unwrap().to_rfc3339(), "2025-12-01T00:00:00.000Z");
        assert_eq!(end.unwrap().to_rfc3339(), "2026-03-01T00:00:00.000Z");
    }

    #[test]
    fn relative_units_resolve() {
        let now = ts("2026-05-31T10:00:00.000Z");
        let (s, e) = resolve_range(&DateRange::Relative(RelativeRange::LastDays(7)), now);
        assert_eq!(s.unwrap().to_rfc3339(), "2026-05-24T10:00:00.000Z");
        assert_eq!(e, None);
        // Day clamps: May 31 − 3 months → Feb 28 (2026 is not a leap year).
        let (s, _) = resolve_range(&DateRange::Relative(RelativeRange::LastMonths(3)), now);
        assert_eq!(s.unwrap().to_rfc3339(), "2026-02-28T10:00:00.000Z");
        let (s, e) = resolve_range(
            &DateRange::Relative(RelativeRange::Month {
                month: 3,
                year: None,
            }),
            now,
        );
        assert_eq!(s.unwrap().to_rfc3339(), "2026-03-01T00:00:00.000Z");
        assert_eq!(e.unwrap().to_rfc3339(), "2026-04-01T00:00:00.000Z");
        // "in December" said in May → December last year.
        let (s, e) = resolve_range(
            &DateRange::Relative(RelativeRange::Month {
                month: 12,
                year: None,
            }),
            now,
        );
        assert_eq!(s.unwrap().to_rfc3339(), "2025-12-01T00:00:00.000Z");
        assert_eq!(e.unwrap().to_rfc3339(), "2026-01-01T00:00:00.000Z");
        let (s, e) = resolve_range(&DateRange::Relative(RelativeRange::Year(2024)), now);
        assert_eq!(s.unwrap().to_rfc3339(), "2024-01-01T00:00:00.000Z");
        assert_eq!(e.unwrap().to_rfc3339(), "2025-01-01T00:00:00.000Z");
    }

    #[test]
    fn like_escaping() {
        assert_eq!(like_escape("a%b_c\\d"), "a\\%b\\_c\\\\d");
    }
}
