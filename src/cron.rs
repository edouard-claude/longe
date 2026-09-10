//! A minimal 5-field cron matcher (minute hour day-of-month month day-of-week),
//! local time. Supports `*`, lists, ranges and steps. No dependency, no surprises.

use chrono::{Datelike, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    minute: Vec<u8>,
    hour: Vec<u8>,
    dom: Vec<u8>,
    month: Vec<u8>,
    dow: Vec<u8>,
    dom_any: bool,
    dow_any: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CronError {
    #[error("cron expression needs 5 fields, got {0}")]
    FieldCount(usize),
    #[error("invalid cron field `{0}`")]
    Field(String),
    #[error("value {0} out of range {1}..={2}")]
    Range(u32, u32, u32),
}

fn parse_field(field: &str, min: u32, max: u32) -> Result<Vec<u8>, CronError> {
    let mut out = Vec::new();
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .map_err(|_| CronError::Field(part.into()))?,
            ),
            None => (part, 1),
        };
        if step == 0 {
            return Err(CronError::Field(part.into()));
        }
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                a.parse().map_err(|_| CronError::Field(part.into()))?,
                b.parse().map_err(|_| CronError::Field(part.into()))?,
            )
        } else {
            let v: u32 = range.parse().map_err(|_| CronError::Field(part.into()))?;
            if part.contains('/') {
                (v, max)
            } else {
                (v, v)
            }
        };
        if lo < min || hi > max || lo > hi {
            return Err(CronError::Range(if lo < min { lo } else { hi }, min, max));
        }
        let mut v = lo;
        while v <= hi {
            let byte = u8::try_from(v).map_err(|_| CronError::Range(v, min, max))?;
            out.push(byte);
            v += step;
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

impl Schedule {
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronError::FieldCount(fields.len()));
        }
        let mut dow = parse_field(fields[4], 0, 7)?;
        // 7 is Sunday too.
        if dow.contains(&7) {
            dow.retain(|d| *d != 7);
            if !dow.contains(&0) {
                dow.insert(0, 0);
            }
        }
        Ok(Self {
            minute: parse_field(fields[0], 0, 59)?,
            hour: parse_field(fields[1], 0, 23)?,
            dom: parse_field(fields[2], 1, 31)?,
            month: parse_field(fields[3], 1, 12)?,
            dow,
            dom_any: fields[2] == "*",
            dow_any: fields[4] == "*",
        })
    }

    /// Does this minute match? Standard cron semantics: when both day fields are
    /// restricted, either one matching is enough.
    pub fn matches<T: Datelike + Timelike>(&self, t: &T) -> bool {
        let minute = u8::try_from(t.minute()).unwrap_or(u8::MAX);
        let hour = u8::try_from(t.hour()).unwrap_or(u8::MAX);
        let dom = u8::try_from(t.day()).unwrap_or(u8::MAX);
        let month = u8::try_from(t.month()).unwrap_or(u8::MAX);
        let dow = u8::try_from(t.weekday().num_days_from_sunday()).unwrap_or(u8::MAX);
        if !self.minute.contains(&minute)
            || !self.hour.contains(&hour)
            || !self.month.contains(&month)
        {
            return false;
        }
        let dom_ok = self.dom.contains(&dom);
        let dow_ok = self.dow.contains(&dow);
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => dom_ok,
            (true, false) => dow_ok,
            (false, false) => dom_ok || dow_ok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    #[test]
    fn every_minute() {
        let s = Schedule::parse("* * * * *").unwrap();
        assert!(s.matches(&at(2026, 9, 9, 23, 30)));
    }

    #[test]
    fn fixed_time() {
        let s = Schedule::parse("30 23 9 9 *").unwrap();
        assert!(s.matches(&at(2026, 9, 9, 23, 30)));
        assert!(!s.matches(&at(2026, 9, 9, 23, 31)));
        assert!(!s.matches(&at(2026, 9, 10, 23, 30)));
    }

    #[test]
    fn steps_and_ranges() {
        let s = Schedule::parse("*/15 9-17 * * 1-5").unwrap();
        // 2026-09-09 is a Wednesday.
        assert!(s.matches(&at(2026, 9, 9, 9, 45)));
        assert!(!s.matches(&at(2026, 9, 9, 9, 50)));
        assert!(!s.matches(&at(2026, 9, 9, 18, 0)));
        // 2026-09-12 is a Saturday.
        assert!(!s.matches(&at(2026, 9, 12, 10, 0)));
    }

    #[test]
    fn sunday_as_seven() {
        let s = Schedule::parse("0 0 * * 7").unwrap();
        // 2026-09-13 is a Sunday.
        assert!(s.matches(&at(2026, 9, 13, 0, 0)));
    }

    #[test]
    fn errors() {
        assert_eq!(
            Schedule::parse("* * * *").unwrap_err(),
            CronError::FieldCount(4)
        );
        assert!(matches!(
            Schedule::parse("60 * * * *").unwrap_err(),
            CronError::Range(60, 0, 59)
        ));
        assert!(matches!(
            Schedule::parse("a * * * *").unwrap_err(),
            CronError::Field(_)
        ));
        assert!(matches!(
            Schedule::parse("*/0 * * * *").unwrap_err(),
            CronError::Field(_)
        ));
    }
}
