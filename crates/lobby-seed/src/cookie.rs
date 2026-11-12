//! The one cookie the seeding transaction keeps: the backend's host-only
//! secure refresh cookie, so that the session it stands for can be ended.

use crate::COOKIE;

/// The longest cookie value kept.
const MAX_VALUE: usize = 1024;

/// What one `Set-Cookie` header does to the kept cookie.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Set(String),
    Clear,
}

/// Read one `Set-Cookie` header. Only the exact refresh cookie counts: by
/// name, without a domain, for the whole site and marked secure. Its
/// lifetime is absent or a number of seconds; one that is not positive
/// clears the cookie. Anything else changes nothing.
pub(crate) fn change(header: &str) -> Option<Change> {
    let mut parts = header.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    if name.trim() != COOKIE {
        return None;
    }
    let (mut whole, mut secure, mut seconds) = (false, false, None);
    for part in parts {
        let (key, attribute) = part.split_once('=').unwrap_or((part, ""));
        match key.trim().to_ascii_lowercase().as_str() {
            // A named domain is not host-only, and a lifetime given as a
            // date is not one this backend states.
            "domain" | "expires" => return None,
            "path" => whole = attribute.trim() == "/",
            "secure" => secure = true,
            "max-age" => seconds = Some(attribute.trim().parse::<i64>().ok()?),
            _ => {}
        }
    }
    if !whole || !secure {
        return None;
    }
    let value = value.trim();
    match seconds {
        Some(seconds) if seconds <= 0 => Some(Change::Clear),
        _ if value.len() <= MAX_VALUE => Some(Change::Set(value.to_owned())),
        _ => None,
    }
}

/// Apply every header of one response: a clearing header takes effect
/// before any header that sets the cookie, wherever it stands, and the last
/// one that sets it wins.
pub(crate) fn apply(kept: &mut Option<String>, headers: &[String]) {
    let changes: Vec<Change> = headers.iter().filter_map(|header| change(header)).collect();
    if changes.contains(&Change::Clear) {
        *kept = None;
    }
    for change in changes {
        if let Change::Set(value) = change {
            *kept = Some(value);
        }
    }
}
