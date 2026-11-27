use super::*;

// ---- Python helpers

/// Pairs as a Python dict builds them: the first place and the last value of each key.
pub(super) fn dict(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in pairs {
        match out.iter_mut().find(|(key, _)| *key == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
    }
    out
}

pub(super) fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `frontmatter(p)[0] if p.exists() else {}`.
pub(super) fn meta(p: &Path) -> Res<Vec<(String, String)>> {
    if !p.exists() {
        return Ok(Vec::new());
    }
    frontmatter(p)
        .map(|(m, _)| dict(m))
        .map_err(|e| fail(os_error(&e, Some(p)), 1))
}

pub(super) fn write_file(p: &Path, text: &str) -> Res<()> {
    write_atomic(p, text.as_bytes()).map_err(|e| fail(os_error(&e, Some(p)), 1))
}

pub(super) fn mkdirs(p: &Path) -> Res<()> {
    fs::create_dir_all(p).map_err(|e| fail(os_error(&e, Some(p)), 1))
}

pub(super) fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

pub(super) fn is_dir(p: &str) -> bool {
    Path::new(p).is_dir()
}

pub(super) fn read_json(p: &Path) -> Option<Value> {
    loads(&read_text(p).ok()?).ok()
}

/// Python `v == n` on a JSON value: a number equal to n, or a bool, which is an int.
pub(super) fn eq_int(v: Option<&Value>, n: i64) -> bool {
    match v {
        Some(Value::Number(x)) => x.as_f64() == Some(n as f64),
        Some(Value::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

/// `type(v) is int`, as an i64.
pub(super) fn int_of(v: Option<&Value>) -> Option<i64> {
    v.filter(|v| v.is_i64() || v.is_u64())
        .and_then(Value::as_i64)
}

pub(super) fn get_str<'a>(j: &'a Value, key: &str) -> Option<&'a str> {
    j.get(key).and_then(Value::as_str)
}

/// The job's id when it is one pickup may paste into a command.
pub(super) fn safe_id(j: &Value) -> Option<&str> {
    get_str(j, "id").filter(|id| JOB_ID.is_full(id) && !id.contains(".."))
}

/// Python's `str.split()`.
pub(super) fn words(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

pub(super) fn dirname(p: &str) -> String {
    Path::new(p)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(super) fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// This binary, resolved: what the timer runs.
pub(super) fn exe() -> String {
    #[cfg(test)]
    if let Some(e) = FAKES.with(|f| f.borrow().exe.clone()) {
        return e;
    }
    std::env::current_exe()
        .and_then(|p| resolve(&p))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The text a `fail` printed, as `setup_try` reads it.
pub(super) fn message(e: &Exit) -> String {
    strip(&e.stderr).to_string()
}

// ---- OS helpers

/// Six lowercase hex characters from three random bytes: `secrets.token_hex(3)`.
pub(super) fn token_hex3() -> String {
    let mut buf = [0u8; 3];
    #[cfg(unix)]
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("read /dev/urandom");
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(alg: *mut u8, buf: *mut u8, len: u32, flags: u32) -> i32;
        }
        // SAFETY: buf is writable for its 3 bytes, and the system RNG flag needs no algorithm handle.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), 3, 0x2) };
        assert_eq!(status, 0, "BCryptGenRandom failed");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Seconds east of UTC of this host's local time at utc.
#[cfg(unix)]
pub(crate) fn local_offset(utc: i64) -> i64 {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: t points at a valid time_t and tm at writable storage for one struct tm.
    let done = unsafe { localtime_r(&utc, tm.as_mut_ptr()) };
    if done.is_null() {
        return 0;
    }
    // SAFETY: localtime_r filled tm, since it returned non-null.
    unsafe { tm.assume_init() }.gmtoff
}

#[cfg(windows)]
pub(crate) fn local_offset(utc: i64) -> i64 {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        weekday: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            tz: *const u8,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }
    let (y, m, d) = civil(utc.div_euclid(86_400));
    let t = utc.rem_euclid(86_400);
    let at = SystemTime {
        year: y as u16,
        month: m as u16,
        day: d as u16,
        hour: (t / 3600) as u16,
        minute: (t / 60 % 60) as u16,
        second: (t % 60) as u16,
        ..Default::default()
    };
    let mut local = SystemTime::default();
    // SAFETY: both are valid SYSTEMTIMEs, and a null zone means the current one.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &at, &mut local) } == 0 {
        return 0;
    }
    let wall = days_from_civil(local.year.into(), local.month.into(), local.day.into()) * 86_400
        + i64::from(local.hour) * 3600
        + i64::from(local.minute) * 60
        + i64::from(local.second);
    wall - utc
}

/// (utc seconds, local offset) now.
pub(super) fn local_now() -> (i64, i64) {
    let utc = unix_now();
    (utc, local_offset(utc))
}

/// The local date as YYYY-MM-DD.
pub(super) fn local_date(utc: i64, offset: i64) -> String {
    format_stamp(utc + offset)[..10].to_string()
}

pub(super) fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub(super) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// An OS error's text without the ` (os error N)` Rust adds: Python's `e.strerror`.
pub(super) fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(n) => text
            .strip_suffix(&format!(" (os error {n})"))
            .unwrap_or(&text)
            .to_string(),
        None => text,
    }
}

/// Start cmd in its own session so it outlives pickup: `start_new_session=True`.
pub(super) fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        extern "C" {
            fn setsid() -> i32;
        }
        // SAFETY: setsid is async-signal-safe and changes only the forked child.
        unsafe {
            cmd.pre_exec(|| {
                setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x200 | 0x8);
    }
}

#[cfg(unix)]
pub(crate) fn getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { getuid() }
}

#[cfg(not(unix))]
pub(crate) fn getuid() -> u32 {
    0
}

/// Python's `shlex.quote`.
pub(super) fn shlex_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Python's `shlex.join`.
pub(super) fn shlex_join<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|a| shlex_quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Python's `shlex.split` in POSIX mode. None where it raises on an open quote or escape.
pub(super) fn shlex_split(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => out.extend(word.take()),
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('\\' | '"') => w.push(c),
                            c => w.extend(['\\', c]),
                        },
                        c => w.push(c),
                    }
                }
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    out.extend(word);
    Some(out)
}

/// Python's `subprocess.list2cmdline`.
pub(super) fn list2cmdline(argv: &[String]) -> String {
    let mut out = String::new();
    for arg in argv {
        if !out.is_empty() {
            out.push(' ');
        }
        let quote = arg.is_empty() || arg.contains([' ', '\t']);
        if quote {
            out.push('"');
        }
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                c => {
                    out.push_str(&"\\".repeat(slashes));
                    slashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(if quote { slashes * 2 } else { slashes }));
        if quote {
            out.push('"');
        }
    }
    out
}

pub(super) fn plist_escape(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub(super) fn plist_value(out: &mut String, v: &Value, depth: usize) {
    let pad = "\t".repeat(depth);
    match v {
        Value::String(s) => out.push_str(&format!("{pad}<string>{}</string>\n", plist_escape(s))),
        Value::Number(n) => out.push_str(&format!("{pad}<integer>{n}</integer>\n")),
        Value::Bool(b) => out.push_str(&format!("{pad}<{b}/>\n")),
        Value::Array(a) if a.is_empty() => out.push_str(&format!("{pad}<array/>\n")),
        Value::Array(a) => {
            out.push_str(&format!("{pad}<array>\n"));
            for item in a {
                plist_value(out, item, depth + 1);
            }
            out.push_str(&format!("{pad}</array>\n"));
        }
        Value::Object(o) if o.is_empty() => out.push_str(&format!("{pad}<dict/>\n")),
        Value::Object(o) => {
            out.push_str(&format!("{pad}<dict>\n"));
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&format!("{pad}\t<key>{}</key>\n", plist_escape(k)));
                plist_value(out, &o[k], depth + 1);
            }
            out.push_str(&format!("{pad}</dict>\n"));
        }
        Value::Null => {}
    }
}

/// Python's `plistlib.dumps` for strings, integers, arrays and dicts.
pub(super) fn plist_dumps(v: &Value) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    plist_value(&mut out, v, 0);
    out.push_str("</plist>\n");
    out
}

/// Python's `plistlib.loads` for an XML plist of strings, integers, arrays and dicts, else None.
pub(super) fn plist_loads(data: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(data).ok()?;
    let mut tags = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let end = rest[start..].find('>')? + start;
        tags.push((&rest[..start], &rest[start + 1..end]));
        rest = &rest[end + 1..];
    }
    if !strip(rest).is_empty() {
        return None;
    }
    tags.retain(|(_, t)| !t.starts_with('?') && !t.starts_with('!'));
    if !tags.first()?.1.starts_with("plist") || tags.last()?.1 != "/plist" {
        return None;
    }
    let mut i = 1;
    let v = plist_parse(&tags, &mut i)?;
    (i == tags.len() - 1).then_some(v)
}

pub(super) fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let end = rest[at..].find(';')? + at;
        let name = &rest[at + 1..end];
        out.push(match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let n = match name.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => name.strip_prefix('#')?.parse().ok()?,
                };
                char::from_u32(n)?
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

pub(super) fn plist_parse(tags: &[(&str, &str)], i: &mut usize) -> Option<Value> {
    let tag = tags.get(*i)?.1;
    *i += 1;
    let text = |i: &mut usize, name: &str| -> Option<String> {
        let (body, close) = tags.get(*i)?;
        *i += 1;
        (*close == format!("/{name}")).then(|| unescape(body))?
    };
    match tag {
        "string" => text(i, "string").map(Value::from),
        "string/" => Some(Value::from("")),
        "integer" => text(i, "integer")?
            .trim()
            .parse::<i64>()
            .ok()
            .map(Value::from),
        "true/" => Some(Value::Bool(true)),
        "false/" => Some(Value::Bool(false)),
        "array/" => Some(Value::Array(Vec::new())),
        "dict/" => Some(Value::Object(Map::new())),
        "array" => {
            let mut items = Vec::new();
            while tags.get(*i)?.1 != "/array" {
                items.push(plist_parse(tags, i)?);
            }
            *i += 1;
            Some(Value::Array(items))
        }
        "dict" => {
            let mut map = Map::new();
            while tags.get(*i)?.1 != "/dict" {
                if tags[*i].1 != "key" {
                    return None;
                }
                *i += 1;
                let key = text(i, "key")?;
                map.insert(key, plist_parse(tags, i)?);
            }
            *i += 1;
            Some(Value::Object(map))
        }
        _ => None,
    }
}

/// `subprocess.run(argv, input=stdin, capture_output=True, text=True, timeout=timeout)`.
pub(super) fn run_input(argv: &[&str], stdin: &str, timeout: Duration) -> io::Result<Out> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let data = stdin.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&data);
    });
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = writer.join();
    let text = |h: std::thread::JoinHandle<Vec<u8>>| {
        crate::shared::py::universal_newlines(
            String::from_utf8_lossy(&h.join().unwrap()).into_owned(),
        )
    };
    Ok(Out {
        code: status.code().unwrap_or(1),
        stdout: text(out),
        stderr: text(err),
    })
}
