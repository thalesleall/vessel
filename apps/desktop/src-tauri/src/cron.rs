//! Five-field cron expressions (`minute hour day-of-month month day-of-week`) in local time, for
//! snippet schedules.
//!
//! Each field takes `*`, numbers, ranges (`1-5`), lists (`1,15`) and steps (`*/10`, `8-18/2`).
//! Months and weekdays also take English names (`jan`, `mon`); weekday 7 is Sunday, like 0. As in
//! classic cron, when both day fields are restricted a day matching either one counts.
//! Shortcuts: `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`.

use chrono::{
    DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    any_day: bool,
    any_weekday: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn field(
    text: &str,
    what: &str,
    min: u32,
    max: u32,
    names: &[&str],
) -> Result<(u64, bool), String> {
    let value = |s: &str| -> Result<u32, String> {
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
            return Ok(min + i as u32);
        }
        s.parse::<u32>()
            .ok()
            .filter(|v| (min..=max).contains(v))
            .ok_or_else(|| format!("\"{s}\" não é um valor válido para {what} ({min} a {max})"))
    };
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| format!("passo inválido em {what}: \"{s}\""))?,
            ),
            None => (part, 1),
        };
        let (from, to) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (value(a)?, value(b)?);
            if a > b {
                return Err(format!("intervalo invertido em {what}: \"{range}\""));
            }
            (a, b)
        } else {
            let a = value(range)?;
            // `5/15` means "from 5, every 15", as in common cron implementations.
            (a, if part.contains('/') { max } else { a })
        };
        let mut v = from;
        while v <= to {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, text == "*"))
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let expr = expr.trim();
        let expr = match expr.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            _ => expr,
        };
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err("Use cinco campos: minuto hora dia mês dia-da-semana (por exemplo \"0 7 * * 1-5\").".into());
        }
        let (minutes, _) = field(parts[0], "minuto", 0, 59, &[])?;
        let (hours, _) = field(parts[1], "hora", 0, 23, &[])?;
        let (days, any_day) = field(parts[2], "dia do mês", 1, 31, &[])?;
        let (months, _) = field(parts[3], "mês", 1, 12, &MONTHS)?;
        let (mut weekdays, any_weekday) = field(parts[4], "dia da semana", 0, 7, &WEEKDAYS)?;
        if weekdays & (1 << 7) != 0 {
            weekdays |= 1;
        }
        Ok(Self {
            minutes,
            hours: hours as u32,
            days: days as u32,
            months: months as u16,
            weekdays: (weekdays & 0x7f) as u8,
            any_day,
            any_weekday,
        })
    }

    fn day_matches(&self, d: NaiveDate) -> bool {
        let dom = self.days & (1 << d.day()) != 0;
        let dow = self.weekdays & (1 << d.weekday().num_days_from_sunday()) != 0;
        match (self.any_day, self.any_weekday) {
            (true, true) => true,
            (false, true) => dom,
            (true, false) => dow,
            (false, false) => dom || dow,
        }
    }

    /// The first matching minute strictly after `after`, within the next five years.
    pub fn next_after(&self, after: NaiveDateTime) -> Option<NaiveDateTime> {
        let mut t = after.with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let limit = after + Duration::days(5 * 366);
        while t <= limit {
            if self.months & (1 << t.month()) == 0 {
                let (y, m) = if t.month() == 12 {
                    (t.year() + 1, 1)
                } else {
                    (t.year(), t.month() + 1)
                };
                t = NaiveDate::from_ymd_opt(y, m, 1)?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !self.day_matches(t.date()) {
                t = t.date().succ_opt()?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if self.hours & (1 << t.hour()) == 0 {
                t = t.with_minute(0)? + Duration::hours(1);
                continue;
            }
            if self.minutes & (1 << t.minute()) == 0 {
                t += Duration::minutes(1);
                continue;
            }
            return Some(t);
        }
        None
    }

    /// Next run in local time. A time skipped by a daylight-saving jump moves to the next match;
    /// a repeated hour runs once, the first time.
    pub fn next_local(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        let mut t = after.naive_local();
        for _ in 0..1000 {
            t = self.next_after(t)?;
            match Local.from_local_datetime(&t) {
                LocalResult::Single(d) if d > after => return Some(d),
                LocalResult::Ambiguous(d, _) if d > after => return Some(d),
                _ => {}
            }
        }
        None
    }

    /// Times in `(from, to]`, at most `limit` of them.
    pub fn between(
        &self,
        from: DateTime<Local>,
        to: DateTime<Local>,
        limit: usize,
    ) -> Vec<DateTime<Local>> {
        let mut out = Vec::new();
        let mut t = from;
        while out.len() < limit {
            match self.next_local(t) {
                Some(n) if n <= to => {
                    out.push(n);
                    t = n;
                }
                _ => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn next(expr: &str, from: &str) -> String {
        Cron::parse(expr)
            .unwrap()
            .next_after(at(from))
            .unwrap()
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }

    #[test]
    fn every_minute_and_steps() {
        assert_eq!(next("* * * * *", "2026-11-17 10:00"), "2026-11-17 10:01");
        assert_eq!(next("*/15 * * * *", "2026-11-17 10:01"), "2026-11-17 10:15");
        assert_eq!(next("*/15 * * * *", "2026-11-17 10:59"), "2026-11-17 11:00");
        assert_eq!(next("5/20 * * * *", "2026-11-17 10:26"), "2026-11-17 10:45");
        assert_eq!(
            next("0 8-18/2 * * *", "2026-11-17 18:00"),
            "2026-11-18 08:00"
        );
    }

    #[test]
    fn weekdays_months_and_names() {
        // 2026-11-17 is a Tuesday.
        assert_eq!(next("0 7 * * 1-5", "2026-11-20 08:00"), "2026-11-23 07:00");
        assert_eq!(
            next("0 7 * * mon-fri", "2026-11-20 08:00"),
            "2026-11-23 07:00"
        );
        assert_eq!(next("30 22 * * 7", "2026-11-17 10:00"), "2026-11-22 22:30");
        assert_eq!(next("0 0 1 jan *", "2026-11-17 10:00"), "2027-01-01 00:00");
        assert_eq!(next("@monthly", "2026-11-17 10:00"), "2026-12-01 00:00");
        assert_eq!(next("0 12 31 * *", "2026-11-17 10:00"), "2026-12-31 12:00");
        assert_eq!(next("0 0 29 2 *", "2026-11-17 10:00"), "2028-02-29 00:00");
    }

    #[test]
    fn both_day_fields_match_either() {
        // The 1st of the month or any Friday.
        assert_eq!(next("0 9 1 * 5", "2026-11-17 10:00"), "2026-11-20 09:00");
        assert_eq!(next("0 9 1 * 5", "2026-11-28 10:00"), "2026-12-01 09:00");
    }

    #[test]
    fn invalid_expressions_explain_why() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
        assert!(Cron::parse("61 * * * *").unwrap_err().contains("minuto"));
    }

    #[test]
    fn impossible_dates_never_match() {
        assert_eq!(
            Cron::parse("0 0 31 2 *")
                .unwrap()
                .next_after(at("2026-01-01 00:00")),
            None
        );
    }

    #[test]
    fn between_lists_each_time_once() {
        let c = Cron::parse("*/10 * * * *").unwrap();
        let from = Local.from_local_datetime(&at("2026-06-10 10:00")).unwrap();
        let to = Local.from_local_datetime(&at("2026-06-10 11:00")).unwrap();
        let times = c.between(from, to, 100);
        assert_eq!(times.len(), 6);
        assert_eq!(times.last().unwrap().naive_local(), at("2026-06-10 11:00"));
        assert_eq!(c.between(from, to, 2).len(), 2);
    }
}
