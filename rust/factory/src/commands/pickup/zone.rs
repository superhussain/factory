use super::*;

/// The TZif data for zone: (transition times, their type index, (utoff, isdst) types, footer).
pub(super) struct Zone {
    pub(super) times: Vec<i64>,
    pub(super) index: Vec<usize>,
    pub(super) types: Vec<(i64, bool)>,
    pub(super) rule: Option<TzRule>,
}

/// A POSIX TZ footer: standard and DST offsets east of UTC, and the DST start and end rules.
pub(super) struct TzRule {
    pub(super) std: i64,
    pub(super) dst: Option<(i64, (u32, u32, u32, u32, i64), (u32, u32, u32, u32, i64))>,
}

pub(super) fn read_zone(name: &str) -> Option<Zone> {
    if Path::new(name).is_absolute() || name.contains("..") || WINDOWS {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("TZDIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    dirs.extend(
        [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/usr/share/lib/zoneinfo",
            "/etc/zoneinfo",
        ]
        .map(PathBuf::from),
    );
    let data = dirs.iter().find_map(|d| fs::read(d.join(name)).ok())?;
    parse_tzif(&data)
}

pub(super) fn parse_tzif(data: &[u8]) -> Option<Zone> {
    let header = |at: usize| -> Option<[usize; 6]> {
        if data.get(at..at + 4)? != b"TZif" {
            return None;
        }
        let mut counts = [0usize; 6];
        for (i, c) in counts.iter_mut().enumerate() {
            let b = data.get(at + 20 + i * 4..at + 24 + i * 4)?;
            *c = u32::from_be_bytes(b.try_into().ok()?) as usize;
        }
        Some(counts)
    };
    let [isut, isstd, leap, time, typ, chars] = header(0)?;
    let v1 = 44 + time * 5 + typ * 6 + chars + leap * 8 + isstd + isut;
    let (start, size, counts) = if data[4] >= b'2' {
        (v1 + 44, 8, header(v1)?)
    } else {
        (44, 4, [isut, isstd, leap, time, typ, chars])
    };
    let [isut, isstd, leap, time, typ, chars] = counts;
    let mut at = start;
    let mut times = Vec::new();
    for _ in 0..time {
        let b = data.get(at..at + size)?;
        times.push(if size == 8 {
            i64::from_be_bytes(b.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(b.try_into().ok()?))
        });
        at += size;
    }
    let index: Vec<usize> = data
        .get(at..at + time)?
        .iter()
        .map(|b| *b as usize)
        .collect();
    at += time;
    let mut types = Vec::new();
    for _ in 0..typ {
        let b = data.get(at..at + 6)?;
        types.push((
            i64::from(i32::from_be_bytes(b[..4].try_into().ok()?)),
            b[4] != 0,
        ));
        at += 6;
    }
    if index.iter().any(|i| *i >= types.len()) || types.is_empty() {
        return None;
    }
    at += chars + leap * (size + 4) + isstd + isut;
    let rule = if size == 8 {
        let footer = std::str::from_utf8(data.get(at..)?).ok()?;
        let footer = footer.strip_prefix('\n')?.split('\n').next()?;
        parse_tz_rule(footer)
    } else {
        None
    };
    Some(Zone {
        times,
        index,
        types,
        rule,
    })
}

/// A POSIX TZ string such as `EST5EDT,M3.2.0,M11.1.0`, or None when it is empty or unsupported.
pub(super) fn parse_tz_rule(s: &str) -> Option<TzRule> {
    fn name(s: &str) -> Option<&str> {
        if let Some(rest) = s.strip_prefix('<') {
            return Some(&rest[rest.find('>')? + 1..]);
        }
        let n = s
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(s.len());
        (n >= 3).then(|| &s[n..])
    }
    fn offset(s: &str) -> Option<(i64, &str)> {
        let (sign, body) = match s.as_bytes().first()? {
            b'-' => (-1, &s[1..]),
            b'+' => (1, &s[1..]),
            _ => (1, s),
        };
        let end = body
            .find(|c: char| !(c.is_ascii_digit() || c == ':'))
            .unwrap_or(body.len());
        let mut secs = 0;
        for (i, part) in body[..end].split(':').enumerate() {
            secs += part.parse::<i64>().ok()? * [3600, 60, 1].get(i)?;
        }
        Some((sign * secs, &body[end..]))
    }
    fn date(s: &str) -> Option<((u32, u32, u32, u32, i64), &str)> {
        let end = s.find(',').unwrap_or(s.len());
        let (spec, time) = s[..end]
            .split_once('/')
            .map_or((&s[..end], None), |(a, b)| (a, Some(b)));
        let time = match time {
            Some(t) => offset(t).filter(|(_, rest)| rest.is_empty())?.0,
            None => 7200,
        };
        let rest = &s[end..];
        let parsed = if let Some(m) = spec.strip_prefix('M') {
            let mut parts = m.split('.').map(|p| p.parse::<u32>().ok());
            (0, parts.next()??, parts.next()??, parts.next()??, time)
        } else if let Some(j) = spec.strip_prefix('J') {
            (1, j.parse().ok()?, 0, 0, time)
        } else {
            (2, spec.parse().ok()?, 0, 0, time)
        };
        Some((parsed, rest))
    }
    let rest = name(s)?;
    let (std, rest) = offset(rest)?;
    if rest.is_empty() {
        return Some(TzRule {
            std: -std,
            dst: None,
        });
    }
    let rest = name(rest)?;
    let (dst, rest) = match rest.as_bytes().first() {
        Some(b',') | None => (std - 3600, rest),
        _ => offset(rest)?,
    };
    let rest = rest.strip_prefix(',')?;
    let (start, rest) = date(rest)?;
    let (end, rest) = date(rest.strip_prefix(',')?)?;
    rest.is_empty().then_some(TzRule {
        std: -std,
        dst: Some((-dst, start, end)),
    })
}

/// Seconds since 1970 of a rule's transition in year y, as local wall time.
pub(super) fn rule_wall(y: i64, (kind, a, b, c, time): (u32, u32, u32, u32, i64)) -> i64 {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let day = match kind {
        0 => {
            let first = days_from_civil(y, i64::from(a), 1);
            let weekday = (first + 4).rem_euclid(7);
            let mut day = first + (i64::from(c) - weekday).rem_euclid(7) + 7 * (i64::from(b) - 1);
            while civil(day).1 != i64::from(a) {
                day -= 7;
            }
            day
        }
        1 => {
            let n = i64::from(a);
            days_from_civil(y, 1, 1) + n - 1 + i64::from(leap && n >= 60)
        }
        _ => days_from_civil(y, 1, 1) + i64::from(a),
    };
    day * 86_400 + time
}

/// Seconds east of UTC in zone at utc, from its TZif data and then its footer rule.
pub(super) fn zone_offset(zone: &str, utc: i64) -> Option<i64> {
    let z = read_zone(zone)?;
    Some(zone_at(&z, utc))
}

pub(super) fn zone_at(z: &Zone, utc: i64) -> i64 {
    let after_last = z.times.last().is_none_or(|last| utc >= *last);
    if let (true, Some(rule)) = (after_last, &z.rule) {
        let Some((dst, start, end)) = rule.dst else {
            return rule.std;
        };
        let y = civil((utc + rule.std).div_euclid(86_400)).0;
        let begin = rule_wall(y, start) - rule.std;
        let finish = rule_wall(y, end) - dst;
        let in_dst = if begin < finish {
            (begin..finish).contains(&utc)
        } else {
            !(finish..begin).contains(&utc)
        };
        return if in_dst { dst } else { rule.std };
    }
    match z.times.iter().rposition(|t| *t <= utc) {
        Some(i) => z.types[z.index[i]].0,
        None => z.types.iter().find(|t| !t.1).unwrap_or(&z.types[0]).0,
    }
}

/// The UTC instant of a wall time in zone, with fold 0: the offset before a transition wins in
/// both an overlap and a gap.
pub(super) fn zone_wall_to_utc(zone: &str, wall: i64) -> Option<i64> {
    let z = read_zone(zone)?;
    let early = zone_at(&z, wall - 14 * 3600);
    let late = zone_at(&z, wall + 14 * 3600);
    if zone_at(&z, wall - early) == early {
        return Some(wall - early);
    }
    if zone_at(&z, wall - late) == late {
        return Some(wall - late);
    }
    Some(wall - early)
}

/// Minutes until a usage limit in launcher output resets, from 1 to 1440, else 60.
pub(super) fn limit_minutes(text: &str, now: i64) -> i64 {
    let mut minutes = 60;
    if let Some(after) = RESET_IN.search(text) {
        let n: i64 = after.get(1).unwrap().as_str().parse().unwrap();
        let hours = after
            .get(2)
            .unwrap()
            .as_str()
            .to_lowercase()
            .starts_with('h');
        minutes = n * if hours { 60 } else { 1 };
    } else if let Some(at) = RESET_AT.search(text) {
        let group = |i| at.get(i).map(|m| m.as_str());
        let reset = || -> Option<i64> {
            let zone = group(6)?;
            let local = now + zone_offset(zone, now)?;
            let (y, m, d) = civil(local.div_euclid(86_400));
            let hour =
                group(3)?.parse::<i64>().ok()? % 12 + if group(5) == Some("pm") { 12 } else { 0 };
            let minute = group(4).map_or(Some(0), |m| m.parse::<i64>().ok())?;
            if minute > 59 {
                return None;
            }
            let wall = |y: i64, m: i64, d: i64| -> Option<i64> {
                let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
                let len = [
                    31,
                    if leap { 29 } else { 28 },
                    31,
                    30,
                    31,
                    30,
                    31,
                    31,
                    30,
                    31,
                    30,
                    31,
                ];
                if d < 1 || d > len[(m - 1) as usize] {
                    return None;
                }
                zone_wall_to_utc(
                    zone,
                    days_from_civil(y, m, d) * 86_400 + hour * 3600 + minute * 60,
                )
            };
            match group(1) {
                None => {
                    let reset = wall(y, m, d)?;
                    if reset > now {
                        return Some(reset);
                    }
                    let (y, m, d) = civil(days_from_civil(y, m, d) + 1);
                    wall(y, m, d)
                }
                Some(month) => {
                    let month = MONTHS.iter().position(|n| *n == month)? as i64 + 1;
                    let day: i64 = group(2)?.parse().ok()?;
                    let reset = wall(y, month, day)?;
                    if reset < now - 86_400 {
                        return wall(y + 1, month, day);
                    }
                    Some(reset)
                }
            }
        };
        if let Some(reset) = reset() {
            minutes = (reset - now + 59).div_euclid(60);
        }
    }
    minutes.clamp(1, 1440)
}
