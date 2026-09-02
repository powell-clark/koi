//! Content-aware filing rules (TASK-KOI246).
//!
//! Koi's original rules were match arms on extension, so `Documents/PDFs` held
//! bank statements, patient statements, invoices and a certificate side by
//! side. Real files carry their meaning in their names, and this module reads
//! it: an ordered rule table matched against the filename, first match wins,
//! with the extension buckets left intact as the fallback for anything the
//! table does not claim.
//!
//! # Order is load-bearing
//!
//! Measured on the real corpus under TASK-KOI245 AC-6: with Finance evaluated
//! before Health, all three `patient-statement-*.pdf` files matched on the word
//! "statement" and routed to `Finance/Statements`. A negative lookahead does
//! not save it — "patient" *precedes* "statement" in those names. The seed
//! table below is therefore ordered specific-to-general, with Health ahead of
//! Finance, and [`RuleSet::first_match`] stops at the first hit rather than
//! scoring every rule.
//!
//! # Why a glob and not a regex
//!
//! The patterns this needs are `*passport*`, `Invoice-*`, `Monzo*statement*`.
//! A five-line glob covers all of them; a regex crate is a large dependency to
//! add to a public repo for no expressive gain here.

use serde::Deserialize;

/// How much consent a rule's proposals need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RuleTier {
    /// Ordinary filing — the standard approve loop.
    Safe,
    /// Content-bearing: a statement, a medical result, an identity document.
    /// Held back from `approve --all` per TASK-KOI229, because on 2026-08-25 a
    /// single `--all` swept 334 personal documents in about 160ms.
    #[default]
    Content,
}

/// One ordered filing rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FilingRule {
    /// Glob matched case-insensitively against the filename (not the path).
    /// `*` matches any run of characters, `?` matches one.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Optional issuer token that must also appear in the filename. Lets one
    /// pattern serve several issuers without duplicating the glob.
    #[serde(default)]
    pub issuer: Option<String>,
    /// A taxonomy destination key from TASK-KOI245.
    pub destination: String,
    #[serde(default)]
    pub tier: RuleTier,
}

impl FilingRule {
    fn matches(&self, filename_lower: &str) -> bool {
        if let Some(issuer) = &self.issuer {
            if !filename_lower.contains(&issuer.to_lowercase()) {
                return false;
            }
        }
        glob_match(&self.pattern.to_lowercase(), filename_lower)
    }
}

/// Case-folded glob: `*` any run (including empty), `?` exactly one char.
///
/// Iterative with backtracking rather than recursive, so a pathological
/// pattern cannot blow the stack on a filename from an untrusted archive.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut resume) = (usize::MAX, 0usize);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            resume = ti;
            pi += 1;
        } else if star != usize::MAX {
            // Backtrack: let the last `*` swallow one more character.
            pi = star + 1;
            resume += 1;
            ti = resume;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// An ordered rule table. First match wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSet {
    rules: Vec<FilingRule>,
}

impl RuleSet {
    pub fn new(rules: Vec<FilingRule>) -> Self {
        Self { rules }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[FilingRule] {
        &self.rules
    }

    /// The first rule claiming this filename, or `None` to fall through to the
    /// extension buckets.
    pub fn first_match(&self, filename: &str) -> Option<&FilingRule> {
        let lower = filename.to_lowercase();
        self.rules.iter().find(|r| r.matches(&lower))
    }

    /// The shipped seed table, ordered specific-to-general.
    ///
    /// Health precedes Finance deliberately — see the module docs.
    pub fn seed() -> Self {
        let r = |pattern: &str, destination: &str, tier: RuleTier| FilingRule {
            pattern: pattern.to_string(),
            issuer: None,
            destination: destination.to_string(),
            tier,
        };
        Self::new(vec![
            // Health first: "patient-statement" contains "statement".
            r("*patient*statement*", "Health", RuleTier::Content),
            r("*prescription*", "Health", RuleTier::Content),
            r("*nhs*", "Health", RuleTier::Content),
            // Identity before anything that might claim an image.
            r("*passport*", "Personal/Identity", RuleTier::Content),
            r("*driving*licence*", "Personal/Identity", RuleTier::Content),
            r(
                "*birth*certificate*",
                "Personal/Identity",
                RuleTier::Content,
            ),
            // Tax and Companies House before generic finance wording.
            r(
                "*companies*house*",
                "Finance/Tax-and-Companies-House",
                RuleTier::Content,
            ),
            r(
                "financials-and-companies*",
                "Finance/Tax-and-Companies-House",
                RuleTier::Content,
            ),
            r(
                "*hmrc*",
                "Finance/Tax-and-Companies-House",
                RuleTier::Content,
            ),
            r(
                "*self*assessment*",
                "Finance/Tax-and-Companies-House",
                RuleTier::Content,
            ),
            // Issuer-named statements.
            r("*monzo*", "Finance/Statements", RuleTier::Content),
            r("*starling*", "Finance/Statements", RuleTier::Content),
            r("*hsbc*", "Finance/Statements", RuleTier::Content),
            r("*bank*statement*", "Finance/Statements", RuleTier::Content),
            r(
                "*certifiedstatement*",
                "Finance/Statements",
                RuleTier::Content,
            ),
            // Invoices and receipts.
            r("invoice*", "Finance/Invoices", RuleTier::Content),
            r("*_invoice*", "Finance/Invoices", RuleTier::Content),
            r("*-invoice*", "Finance/Invoices", RuleTier::Content),
            r("receipt*", "Finance/Receipts", RuleTier::Content),
            r("*-receipt*", "Finance/Receipts", RuleTier::Content),
            // Travel.
            r("*boarding*pass*", "Personal/Travel", RuleTier::Content),
            r("*itinerary*", "Personal/Travel", RuleTier::Content),
            // Screenshots before the generic image extensions.
            r("screenshot*", "Media/Screenshots", RuleTier::Safe),
            r("screen shot*", "Media/Screenshots", RuleTier::Safe),
            // Extension-shaped rules for the loose set measured 2026-09-02.
            r("*.ttf", "Fonts", RuleTier::Safe),
            r("*.otf", "Fonts", RuleTier::Safe),
            r("*.woff", "Fonts", RuleTier::Safe),
            r("*.woff2", "Fonts", RuleTier::Safe),
            r("*.deb", "Software", RuleTier::Safe),
            r("*.appimage", "Software", RuleTier::Safe),
            r("*.msi", "Software", RuleTier::Safe),
            r("*.exe", "Software", RuleTier::Safe),
            r("*.dmg", "Software", RuleTier::Safe),
            // Last, and only what no earlier rule claimed: the loose document
            // formats measured in Downloads and inbox on 2026-09-02 that had
            // no extension bucket at all and so produced no proposal.
            r("*.csv", "Reference", RuleTier::Content),
            r("*.docx", "Reference", RuleTier::Content),
            r("*.txt", "Reference", RuleTier::Content),
        ])
    }
}

impl Default for RuleSet {
    fn default() -> Self {
        Self::seed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_handles_star_question_and_anchors() {
        assert!(glob_match("*passport*", "epc passport.pdf"));
        assert!(glob_match("invoice*", "invoice tb-epc-031.pdf"));
        assert!(!glob_match("invoice*", "my invoice.pdf"));
        assert!(glob_match("*.ttf", "inter-regular.ttf"));
        assert!(!glob_match("*.ttf", "inter-regular.otf"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", "anything at all"));
    }

    #[test]
    fn glob_backtracks_rather_than_giving_up() {
        // The naive single-pass matcher fails this: the first `*` must give
        // back characters once `statement` fails to line up at its first try.
        assert!(glob_match("*a*statement*", "banking-statement-2026.pdf"));
    }

    #[test]
    fn patient_statement_routes_to_health_not_finance() {
        // The exact collision TASK-KOI245 AC-6 measured on the real corpus.
        let set = RuleSet::seed();
        let hit = set
            .first_match("patient-statement-emmanuel-powell-clark-2026.pdf")
            .expect("a patient statement must match a rule");
        assert_eq!(hit.destination, "Health");
    }

    #[test]
    fn seed_rules_cover_the_flagged_set_from_task_koi214() {
        let set = RuleSet::seed();
        for (filename, expected) in [
            ("Monzo_bank_statement_2026-01.pdf", "Finance/Statements"),
            ("StarlingCertifiedStatement_2026.pdf", "Finance/Statements"),
            (
                "Monzo_Credit_Agreement_2021-08-10.pdf",
                "Finance/Statements",
            ),
            ("Invoice TB-EPC-031.pdf", "Finance/Invoices"),
            ("invoice-PCL-2026-001.pdf", "Finance/Invoices"),
            ("Receipt-2613-6471.pdf", "Finance/Receipts"),
            ("patient-statement-emmanuel-powell-clark.pdf", "Health"),
            ("passport.jpg", "Personal/Identity"),
            ("EPC Passport.pdf", "Personal/Identity"),
            (
                "financials-and-companies-house-20251031T175423Z-1-001.zip",
                "Finance/Tax-and-Companies-House",
            ),
        ] {
            let hit = set
                .first_match(filename)
                .unwrap_or_else(|| panic!("{filename} matched no seed rule"));
            assert_eq!(hit.destination, expected, "wrong bucket for {filename}");
        }
    }

    #[test]
    fn seed_rules_cover_the_loose_extension_set() {
        let set = RuleSet::seed();
        for (filename, expected) in [
            ("Inter-Regular.ttf", "Fonts"),
            ("SomeFont.otf", "Fonts"),
            ("ventoy_1.0.99_amd64.deb", "Software"),
            ("Cursor-3.14.AppImage", "Software"),
        ] {
            let hit = set
                .first_match(filename)
                .unwrap_or_else(|| panic!("{filename} matched no seed rule"));
            assert_eq!(hit.destination, expected);
        }
    }

    #[test]
    fn ac4_document_formats_reach_reference_but_only_last() {
        let set = RuleSet::seed();
        assert_eq!(
            set.first_match("notes.txt").unwrap().destination,
            "Reference"
        );
        assert_eq!(
            set.first_match("data.csv").unwrap().destination,
            "Reference"
        );
        assert_eq!(
            set.first_match("Minutes.docx").unwrap().destination,
            "Reference"
        );
        // An earlier rule still wins over the Reference catch-alls.
        assert_eq!(
            set.first_match("Invoice-2026.docx").unwrap().destination,
            "Finance/Invoices"
        );
    }

    #[test]
    fn unmatched_files_fall_through_so_the_extension_buckets_still_run() {
        let set = RuleSet::seed();
        assert!(set.first_match("holiday-photo-2019.jpg").is_none());
    }

    #[test]
    fn a_mission_statement_is_not_a_bank_statement() {
        // The greedy-glob failure the card's pre-mortem names. `*statement*`
        // would have claimed this for Finance; `*bank*statement*` does not.
        // It lands in Reference via the AC-4 catch-all, which is right — the
        // point of the test is the bucket it must NOT reach.
        let set = RuleSet::seed();
        let hit = set.first_match("mission statement draft.docx").unwrap();
        assert_eq!(hit.destination, "Reference");
        assert_ne!(hit.destination, "Finance/Statements");
    }

    #[test]
    fn issuer_token_narrows_a_shared_pattern() {
        let set = RuleSet::new(vec![FilingRule {
            pattern: "*statement*".to_string(),
            issuer: Some("Monzo".to_string()),
            destination: "Finance/Statements".to_string(),
            tier: RuleTier::Content,
        }]);
        assert!(set.first_match("monzo_statement_jan.pdf").is_some());
        assert!(set.first_match("mission statement.pdf").is_none());
    }

    #[test]
    fn content_tier_is_the_default_for_an_operator_authored_rule() {
        // An operator adding a rule without saying `tier` gets the cautious
        // one, not the sweeping one.
        let rule: FilingRule =
            toml::from_str("match = \"*.pdf\"\ndestination = \"Reference\"").unwrap();
        assert_eq!(rule.tier, RuleTier::Content);
    }
}
