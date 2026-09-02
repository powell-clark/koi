//! Subscription and renewal register (TASK-KOI239).
//!
//! The half of cost posture that needs no API token: the operator's recurring
//! costs are already on disk as receipts koi files. This reads them, proposes
//! a register, and keeps it current.
//!
//! # Two arithmetic traps, both from this card's pre-mortem
//!
//! A one-off invoice read as a subscription inflates the monthly total
//! permanently, so [`infer_cadence`] only calls something recurring when two
//! receipts from the same provider are actually a period apart; a single
//! receipt is [`Cadence::OneOff`] and excluded from the monthly total.
//!
//! Mixed currencies summed blindly produce a number that means nothing.
//! [`monthly_totals`] returns a total PER currency and there is deliberately no
//! function that collapses them — conversion needs a rate, a rate needs a date,
//! and a wrong total here is worse than no total.

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cadence {
    Monthly,
    Yearly,
    /// Charged once. Excluded from every recurring total.
    OneOff,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub name: String,
    pub provider: String,
    pub amount: f64,
    pub currency: String,
    pub cadence: Cadence,
    /// `YYYY-MM-DD`, absent for one-offs and for anything not yet confirmed.
    #[serde(default)]
    pub next_renewal: Option<String>,
    /// Receipt path this row came from, or "manual".
    pub source: String,
    /// Rows seeded from receipts start unconfirmed and are excluded from
    /// totals until the operator confirms them — the same consent shape the
    /// filing loop uses, for the same reason.
    #[serde(default)]
    pub confirmed: bool,
}

/// What a single receipt tells us, before any cadence is inferred.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptFacts {
    pub provider: String,
    pub amount: f64,
    pub currency: String,
    pub issued: NaiveDate,
}

const KNOWN_PROVIDERS: &[(&str, &str)] = &[
    ("anthropic", "Anthropic"),
    ("claude", "Anthropic"),
    ("railway", "Railway"),
    ("vercel", "Vercel"),
    ("github", "GitHub"),
    ("openai", "OpenAI"),
    ("google", "Google"),
    ("cloudflare", "Cloudflare"),
    ("mullvad", "Mullvad"),
];

fn detect_provider(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    KNOWN_PROVIDERS
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(_, name)| (*name).to_string())
}

fn currency_for_symbol(sym: char) -> &'static str {
    match sym {
        '£' => "GBP",
        '$' => "USD",
        '€' => "EUR",
        _ => "USD",
    }
}

/// Pull the amount due, its currency, the provider and the issue date out of
/// receipt text (as produced by `pdftotext`).
///
/// "Amount due" is preferred over "Total" and both over "Subtotal": on the
/// real corpus the subtotal excludes tax, so seeding from it would understate
/// every recurring cost. Where an explicit ISO code follows the figure
/// (`$20.00 USD`) it wins over the symbol, because the symbol alone cannot
/// tell USD from other dollars.
pub fn parse_receipt_text(text: &str) -> Option<ReceiptFacts> {
    let provider = detect_provider(text)?;
    let issued = parse_issue_date(text)?;
    let (amount, currency) = parse_amount_due(text)?;
    Some(ReceiptFacts {
        provider,
        amount,
        currency,
        issued,
    })
}

fn parse_issue_date(text: &str) -> Option<NaiveDate> {
    let line = text
        .lines()
        .find(|l| l.to_lowercase().contains("date of issue"))?;
    let after = line.to_lowercase().replace("date of issue", "");
    let cleaned = after.trim().replace(',', "");
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let month = match parts[0] {
        m if m.starts_with("jan") => 1,
        m if m.starts_with("feb") => 2,
        m if m.starts_with("mar") => 3,
        m if m.starts_with("apr") => 4,
        m if m.starts_with("may") => 5,
        m if m.starts_with("jun") => 6,
        m if m.starts_with("jul") => 7,
        m if m.starts_with("aug") => 8,
        m if m.starts_with("sep") => 9,
        m if m.starts_with("oct") => 10,
        m if m.starts_with("nov") => 11,
        m if m.starts_with("dec") => 12,
        _ => return None,
    };
    let day: u32 = parts[1].parse().ok()?;
    let year: i32 = parts[2].parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
}

fn parse_amount_due(text: &str) -> Option<(f64, String)> {
    // Preference order matters — see the doc comment on parse_receipt_text.
    for needle in ["amount due", "total excluding tax", "total"] {
        for line in text.lines() {
            let lower = line.to_lowercase();
            if !lower.contains(needle) {
                continue;
            }
            // "total excluding tax" also contains "total"; skip it when we are
            // looking for the real total, or the pre-tax figure wins.
            if needle == "total" && lower.contains("excluding tax") {
                continue;
            }
            if needle == "total" && lower.contains("subtotal") {
                continue;
            }
            if let Some(found) = extract_money(line) {
                return Some(found);
            }
        }
    }
    None
}

fn extract_money(line: &str) -> Option<(f64, String)> {
    let chars: Vec<char> = line.chars().collect();
    let idx = chars.iter().position(|c| matches!(c, '£' | '$' | '€'))?;
    let symbol = chars[idx];
    let digits: String = chars[idx + 1..]
        .iter()
        .take_while(|c| c.is_ascii_digit() || **c == '.' || **c == ',')
        .filter(|c| **c != ',')
        .collect();
    let amount: f64 = digits.parse().ok()?;

    // An explicit ISO code after the figure beats the symbol.
    let tail: String = chars[idx..].iter().collect::<String>().to_uppercase();
    let currency = ["USD", "GBP", "EUR", "CAD", "AUD"]
        .iter()
        .find(|code| tail.contains(*code))
        .map_or_else(
            || currency_for_symbol(symbol).to_string(),
            |c| (*c).to_string(),
        );
    Some((amount, currency))
}

/// Infer how often a provider charges from the dates of its receipts.
///
/// One receipt is never enough: a single invoice is a one-off until a second
/// one proves a rhythm. That is the pre-mortem's first failure mode, and it is
/// the difference between a register that overstates the monthly burn forever
/// and one that grows into accuracy.
pub fn infer_cadence(mut issue_dates: Vec<NaiveDate>) -> Cadence {
    if issue_dates.len() < 2 {
        return Cadence::OneOff;
    }
    issue_dates.sort_unstable();
    let gaps: Vec<i64> = issue_dates
        .windows(2)
        .map(|w| (w[1] - w[0]).num_days())
        .filter(|d| *d > 0)
        .collect();
    if gaps.is_empty() {
        return Cadence::OneOff;
    }
    let shortest = *gaps.iter().min().expect("gaps is non-empty");
    match shortest {
        25..=35 => Cadence::Monthly,
        350..=380 => Cadence::Yearly,
        _ => Cadence::OneOff,
    }
}

/// Monthly-equivalent cost of one row, or `None` when it does not recur.
pub fn monthly_equivalent(sub: &Subscription) -> Option<f64> {
    match sub.cadence {
        Cadence::Monthly => Some(sub.amount),
        Cadence::Yearly => Some(sub.amount / 12.0),
        Cadence::OneOff => None,
    }
}

/// Monthly-equivalent totals, one per currency.
///
/// There is deliberately no single-number variant: summing GBP and USD needs a
/// rate, a rate needs a date, and a confidently wrong total is worse than
/// two honest ones.
pub fn monthly_totals(subs: &[Subscription]) -> BTreeMap<String, f64> {
    let mut totals: BTreeMap<String, f64> = BTreeMap::new();
    for sub in subs.iter().filter(|s| s.confirmed) {
        if let Some(m) = monthly_equivalent(sub) {
            *totals.entry(sub.currency.clone()).or_insert(0.0) += m;
        }
    }
    totals
}

/// Rows renewing within `days` of `today`, soonest first.
pub fn renewals_within(subs: &[Subscription], today: NaiveDate, days: i64) -> Vec<&Subscription> {
    let mut due: Vec<(i64, &Subscription)> = subs
        .iter()
        .filter(|s| s.confirmed)
        .filter_map(|s| {
            let date = NaiveDate::parse_from_str(s.next_renewal.as_deref()?, "%Y-%m-%d").ok()?;
            let delta = (date - today).num_days();
            (0..=days).contains(&delta).then_some((delta, s))
        })
        .collect();
    due.sort_by_key(|(d, _)| *d);
    due.into_iter().map(|(_, s)| s).collect()
}

/// The next renewal after `from` for a given cadence, used when seeding.
pub fn next_renewal_after(last_issued: NaiveDate, cadence: Cadence) -> Option<NaiveDate> {
    match cadence {
        Cadence::OneOff => None,
        Cadence::Monthly => add_months(last_issued, 1),
        Cadence::Yearly => add_months(last_issued, 12),
    }
}

/// Add months without overflowing a short month: 31 January plus one month is
/// 28 or 29 February, not 3 March.
fn add_months(date: NaiveDate, months: u32) -> Option<NaiveDate> {
    let total = date.month0() + months;
    let year = date.year() + (total / 12) as i32;
    let month = total % 12 + 1;
    let mut day = date.day();
    loop {
        if let Some(d) = NaiveDate::from_ymd_opt(year, month, day) {
            return Some(d);
        }
        day -= 1;
        if day == 0 {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are redacted reconstructions of the Stripe-style invoice layout
    // the real corpus uses (TASK-KOI214's flagged set). No real receipt text,
    // no real amounts, and nothing personal — this repo is public.
    const ANTHROPIC: &str = "\
Invoice
Date of issue  May 11, 2026
Date due       May 11, 2026
Anthropic PBC
Claude Code Max subscription
                     Subtotal                 £150.00
                     Total excluding tax      £150.00
                     Total                    £180.00
                     Amount due               £180.00
";

    const RAILWAY: &str = "\
Invoice
Date of issue  June 5, 2026
Railway Corp
                     Subtotal                 $40.21
                     Total                    $20.21
                     Amount due               $20.21 USD
";

    const ONE_OFF: &str = "\
Invoice
Date of issue  March 22, 2026
Vercel Inc
                     Subtotal                 $20.00
                     Total                    $20.00
                     Amount due               $20.00 USD
";

    #[test]
    fn extracts_provider_amount_currency_and_date() {
        let f = parse_receipt_text(ANTHROPIC).expect("anthropic receipt should parse");
        assert_eq!(f.provider, "Anthropic");
        assert!((f.amount - 180.00).abs() < f64::EPSILON);
        assert_eq!(f.currency, "GBP");
        assert_eq!(f.issued, NaiveDate::from_ymd_opt(2026, 5, 11).unwrap());
    }

    #[test]
    fn amount_due_beats_subtotal_so_tax_is_not_lost() {
        // The subtotal is £150 and the amount due is £180. Seeding from the
        // subtotal would understate this subscription by 20% forever.
        let f = parse_receipt_text(ANTHROPIC).unwrap();
        assert!((f.amount - 180.00).abs() < f64::EPSILON, "got {}", f.amount);
    }

    #[test]
    fn an_explicit_iso_code_beats_the_symbol() {
        let f = parse_receipt_text(RAILWAY).unwrap();
        assert_eq!(f.currency, "USD");
        assert!((f.amount - 20.21).abs() < f64::EPSILON);
    }

    #[test]
    fn a_third_fixture_parses_as_well() {
        let f = parse_receipt_text(ONE_OFF).unwrap();
        assert_eq!(f.provider, "Vercel");
        assert_eq!(f.issued, NaiveDate::from_ymd_opt(2026, 3, 22).unwrap());
    }

    #[test]
    fn text_with_no_known_provider_yields_nothing_rather_than_a_guess() {
        assert!(parse_receipt_text("Date of issue May 1, 2026\nAmount due $5.00").is_none());
    }

    #[test]
    fn one_receipt_is_a_one_off_not_a_subscription() {
        // The pre-mortem's first failure mode: a single invoice read as
        // recurring inflates the monthly total permanently.
        let d = NaiveDate::from_ymd_opt(2026, 5, 11).unwrap();
        assert_eq!(infer_cadence(vec![d]), Cadence::OneOff);
    }

    #[test]
    fn two_receipts_a_month_apart_are_monthly() {
        let a = NaiveDate::from_ymd_opt(2026, 4, 11).unwrap();
        let b = NaiveDate::from_ymd_opt(2026, 5, 11).unwrap();
        assert_eq!(infer_cadence(vec![b, a]), Cadence::Monthly);
    }

    #[test]
    fn two_receipts_a_year_apart_are_yearly() {
        let a = NaiveDate::from_ymd_opt(2025, 5, 11).unwrap();
        let b = NaiveDate::from_ymd_opt(2026, 5, 11).unwrap();
        assert_eq!(infer_cadence(vec![a, b]), Cadence::Yearly);
    }

    #[test]
    fn an_irregular_gap_is_not_promoted_to_a_cadence() {
        let a = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let b = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
        assert_eq!(infer_cadence(vec![a, b]), Cadence::OneOff);
    }

    fn sub(provider: &str, amount: f64, currency: &str, cadence: Cadence) -> Subscription {
        Subscription {
            name: provider.to_string(),
            provider: provider.to_string(),
            amount,
            currency: currency.to_string(),
            cadence,
            next_renewal: None,
            source: "manual".to_string(),
            confirmed: true,
        }
    }

    #[test]
    fn totals_are_per_currency_and_never_collapsed() {
        // The pre-mortem's second failure mode. 180 GBP + 20 USD is not 200 of
        // anything.
        let subs = vec![
            sub("Anthropic", 180.0, "GBP", Cadence::Monthly),
            sub("Railway", 20.21, "USD", Cadence::Monthly),
            sub("Domain", 12.0, "USD", Cadence::Yearly),
        ];
        let totals = monthly_totals(&subs);
        assert_eq!(totals.len(), 2);
        assert!((totals["GBP"] - 180.0).abs() < f64::EPSILON);
        assert!((totals["USD"] - 21.21).abs() < 1e-9, "yearly must be /12");
    }

    #[test]
    fn one_offs_and_unconfirmed_rows_are_excluded_from_the_total() {
        let mut unconfirmed = sub("Ghost", 99.0, "GBP", Cadence::Monthly);
        unconfirmed.confirmed = false;
        let subs = vec![
            sub("Anthropic", 180.0, "GBP", Cadence::Monthly),
            sub("Hardware", 500.0, "GBP", Cadence::OneOff),
            unconfirmed,
        ];
        let totals = monthly_totals(&subs);
        assert!((totals["GBP"] - 180.0).abs() < f64::EPSILON);
    }

    #[test]
    fn renewals_inside_the_window_come_back_soonest_first() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
        let mut soon = sub("Railway", 20.0, "USD", Cadence::Monthly);
        soon.next_renewal = Some("2026-09-05".to_string());
        let mut later = sub("Anthropic", 180.0, "GBP", Cadence::Monthly);
        later.next_renewal = Some("2026-09-04".to_string());
        let mut outside = sub("Domain", 12.0, "USD", Cadence::Yearly);
        outside.next_renewal = Some("2026-10-01".to_string());

        let subs = [soon, later, outside];
        let due = renewals_within(&subs, today, 7);
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].provider, "Anthropic");
    }

    #[test]
    fn a_renewal_in_the_past_is_not_reported_as_upcoming() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
        let mut overdue = sub("Railway", 20.0, "USD", Cadence::Monthly);
        overdue.next_renewal = Some("2026-08-01".to_string());
        let subs = [overdue];
        assert!(renewals_within(&subs, today, 7).is_empty());
    }

    #[test]
    fn adding_a_month_to_the_31st_lands_in_the_short_month() {
        // 31 January + 1 month must be 28 February, not 3 March.
        let jan31 = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(
            next_renewal_after(jan31, Cadence::Monthly),
            NaiveDate::from_ymd_opt(2026, 2, 28)
        );
    }

    #[test]
    fn a_one_off_has_no_next_renewal() {
        let d = NaiveDate::from_ymd_opt(2026, 5, 11).unwrap();
        assert_eq!(next_renewal_after(d, Cadence::OneOff), None);
    }
}

// -- the register file -----------------------------------------------------

/// The on-disk register: `~/.config/koi/subscriptions.toml`.
///
/// Lives in the runtime home like every other koi setting, and deliberately
/// NOT in the repo — it is a list of what the operator pays for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Register {
    pub subscriptions: Vec<Subscription>,
}

impl Register {
    pub fn default_path() -> crate::Result<std::path::PathBuf> {
        Ok(crate::state::home_dir()?.join(".config/koi/subscriptions.toml"))
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::default_path().map_or_else(|_| Self::default(), |p| Self::load_from(&p))
    }

    pub fn save_to(&self, path: &std::path::Path) -> crate::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| crate::Error::Config(format!("serialise register: {e}")))?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn save(&self) -> crate::Result<()> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    /// Merge seeded candidates in, without ever overwriting a row the operator
    /// has already confirmed. A seeding pass is a proposal, not an authority.
    pub fn merge_candidates(&mut self, candidates: Vec<Subscription>) -> usize {
        let mut added = 0;
        for c in candidates {
            let existing = self
                .subscriptions
                .iter_mut()
                .find(|s| s.provider == c.provider && s.currency == c.currency);
            match existing {
                Some(row) if row.confirmed => {}
                Some(row) => *row = c,
                None => {
                    self.subscriptions.push(c);
                    added += 1;
                }
            }
        }
        added
    }
}

/// Turn a provider's receipts into a candidate register row.
pub fn candidate_from_receipts(provider: &str, facts: &[ReceiptFacts]) -> Option<Subscription> {
    let latest = facts.iter().max_by_key(|f| f.issued)?;
    let cadence = infer_cadence(facts.iter().map(|f| f.issued).collect());
    Some(Subscription {
        name: provider.to_string(),
        provider: provider.to_string(),
        amount: latest.amount,
        currency: latest.currency.clone(),
        cadence,
        next_renewal: next_renewal_after(latest.issued, cadence).map(|d| d.to_string()),
        source: format!("{} receipt(s) on disk", facts.len()),
        confirmed: false,
    })
}

#[cfg(test)]
mod register_tests {
    use super::*;

    fn facts(provider: &str, amount: f64, y: i32, m: u32, d: u32) -> ReceiptFacts {
        ReceiptFacts {
            provider: provider.to_string(),
            amount,
            currency: "GBP".to_string(),
            issued: NaiveDate::from_ymd_opt(y, m, d).unwrap(),
        }
    }

    #[test]
    fn a_candidate_uses_the_latest_amount_and_the_inferred_cadence() {
        let f = vec![
            facts("Anthropic", 150.0, 2026, 4, 11),
            facts("Anthropic", 180.0, 2026, 5, 11),
        ];
        let c = candidate_from_receipts("Anthropic", &f).unwrap();
        assert!((c.amount - 180.0).abs() < f64::EPSILON, "latest price wins");
        assert_eq!(c.cadence, Cadence::Monthly);
        assert_eq!(c.next_renewal.as_deref(), Some("2026-06-11"));
        assert!(!c.confirmed, "seeded rows must start unconfirmed");
    }

    #[test]
    fn seeding_never_overwrites_a_confirmed_row() {
        // A seeding pass is a proposal. If it could overwrite a confirmed row,
        // one odd invoice would silently rewrite what the operator agreed to.
        let mut reg = Register {
            subscriptions: vec![Subscription {
                name: "Anthropic".into(),
                provider: "Anthropic".into(),
                amount: 180.0,
                currency: "GBP".into(),
                cadence: Cadence::Monthly,
                next_renewal: Some("2026-06-11".into()),
                source: "manual".into(),
                confirmed: true,
            }],
        };
        let candidate =
            candidate_from_receipts("Anthropic", &[facts("Anthropic", 9999.0, 2026, 7, 11)])
                .unwrap();
        reg.merge_candidates(vec![candidate]);
        assert!((reg.subscriptions[0].amount - 180.0).abs() < f64::EPSILON);
        assert!(reg.subscriptions[0].confirmed);
    }

    #[test]
    fn register_round_trips_through_toml() {
        let dir = std::env::temp_dir().join(format!("koi-reg-{}", std::process::id()));
        let path = dir.join("subscriptions.toml");
        let reg = Register {
            subscriptions: vec![candidate_from_receipts(
                "Railway",
                &[facts("Railway", 20.21, 2026, 6, 5)],
            )
            .unwrap()],
        };
        reg.save_to(&path).unwrap();
        assert_eq!(Register::load_from(&path), reg);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_register_is_empty_rather_than_an_error() {
        let reg = Register::load_from(std::path::Path::new("/nonexistent/koi/subs.toml"));
        assert!(reg.subscriptions.is_empty());
    }
}
