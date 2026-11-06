//! Rule-based named-entity recognition for the default (mock) analyzer.
//!
//! Deterministic and offline — no model (HLD §56, Invariant 10). Two parts:
//! - typed value recognisers: money, email, URL, phone, IBAN (mod-97
//!   checked), percentage;
//! - capitalised-phrase names, typed from cues: legal-form / organisation
//!   head nouns, honorifics, name particles, and a small built-in gazetteer
//!   of countries, major cities and regions.
//!
//! Every match carries its byte span in the scanned text, so candidates
//! keep `EvidenceSource::TextSpan` provenance.

/// Model id stamped on rule-derived candidates (DoD: model versions persisted).
pub(crate) const RULES_MODEL: &str = "rules-v1";

/// Entity kinds the recognisers emit for typed values. Named kinds are
/// "Organization" / "Person" / "Location" / "Thing".
pub(crate) const VALUE_KINDS: &[&str] = &["Money", "Email", "Url", "Phone", "Iban", "Percentage"];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Match {
    pub name: String,
    pub kind: &'static str,
    /// Byte offsets into the scanned text.
    pub start: usize,
    pub end: usize,
    pub confidence: f32,
}

const LEAD: &[char] = &['(', '[', '{', '"', '\'', '“', '‘', '<'];
const TRAIL: &[char] = &[
    ')', ']', '}', '"', '\'', '”', '’', '>', ',', ';', ':', '.', '!', '?',
];

/// Whitespace tokens: (start, end) byte offsets of the raw token and of its
/// punctuation-trimmed core, plus whether a newline precedes it.
#[derive(Clone, Copy)]
struct Tok {
    start: usize,
    end: usize,
    core_start: usize,
    core_end: usize,
    newline_before: bool,
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut newline = false;
    let push = |s: usize, e: usize, nl: bool, out: &mut Vec<Tok>| {
        let raw = &text[s..e];
        let lead = raw.len() - raw.trim_start_matches(LEAD).len();
        let trimmed = raw.trim_start_matches(LEAD).trim_end_matches(TRAIL);
        let (cs, ce) = if trimmed.is_empty() {
            (s, s)
        } else {
            (s + lead, s + lead + trimmed.len())
        };
        out.push(Tok {
            start: s,
            end: e,
            core_start: cs,
            core_end: ce,
            newline_before: nl,
        });
    };
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                push(s, i, newline, &mut out);
                newline = false;
            }
            if c == '\n' {
                newline = true;
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        push(s, text.len(), newline, &mut out);
    }
    out
}

// ---------------------------------------------------------------- values

fn is_number(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_digit()
        && b[b.len() - 1].is_ascii_digit()
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == ',' || c == '.')
}

const CURRENCY_SYMBOLS: &[char] = &['$', '€', '£', '¥', '₹'];
const CURRENCY_CODES: &[&str] = &[
    "EUR", "USD", "GBP", "JPY", "CHF", "INR", "AUD", "CAD", "CNY", "SEK", "NOK", "DKK", "PLN",
    "NZD", "SGD", "HKD", "ZAR", "BRL", "MXN",
];
const SCALE_WORDS: &[&str] = &[
    "million", "billion", "thousand", "trillion", "mn", "bn", "m", "k",
];

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || domain.contains('@')
        || !local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
    {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    labels.len() >= 2
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels
            .last()
            .is_some_and(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
}

/// ISO 13616 mod-97 check over the compact form.
fn iban_valid(compact: &str) -> bool {
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    let b = compact.as_bytes();
    if !(b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit())
    {
        return false;
    }
    let rearranged = format!("{}{}", &compact[4..], &compact[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let v = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'A'..='Z' => c as u32 - 'A' as u32 + 10,
            _ => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

/// Typed values in `text`, left to right, non-overlapping.
pub(crate) fn recognize_values(text: &str) -> Vec<Match> {
    let toks = tokenize(text);
    let core = |t: &Tok| &text[t.core_start..t.core_end];
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let c = core(&toks[i]);
        let start = toks[i].core_start;
        let mut found: Option<(usize, &'static str, f32)> = None; // (last tok, kind, conf)

        if (c.starts_with("http://") || c.starts_with("https://") || c.starts_with("www."))
            && c.len() > 8
        {
            found = Some((i, "Url", 0.95));
        } else if is_email(c) {
            found = Some((i, "Email", 0.95));
        } else if c.len() >= 4
            && c.chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
            && c.as_bytes()[..2].iter().all(|b| b.is_ascii_uppercase())
            && c.as_bytes()[2..4].iter().all(|b| b.is_ascii_digit())
        {
            // IBAN, compact or in groups of up to four: longest valid prefix.
            let mut compact = c.to_string();
            let mut ends = vec![(i, compact.clone())];
            let mut j = i + 1;
            while j < toks.len() && compact.len() < 34 {
                let g = core(&toks[j]);
                if g.is_empty()
                    || g.len() > 4
                    || !g
                        .chars()
                        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
                {
                    break;
                }
                compact.push_str(g);
                ends.push((j, compact.clone()));
                j += 1;
            }
            if let Some((last, _)) = ends.iter().rev().find(|(_, s)| iban_valid(s)) {
                found = Some((*last, "Iban", 0.95));
            }
        } else if c.starts_with('+') && c.len() > 1 && c[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            // International phone: +CC then digit groups, 8–15 digits total.
            let mut digits = c.len() - 1;
            let mut j = i;
            while j + 1 < toks.len() {
                let g = core(&toks[j + 1]);
                let gd = g.chars().filter(|ch| ch.is_ascii_digit()).count();
                if gd == 0
                    || !g
                        .chars()
                        .all(|ch| ch.is_ascii_digit() || "-()".contains(ch))
                    || digits + gd > 15
                    || toks[j + 1].newline_before
                {
                    break;
                }
                digits += gd;
                j += 1;
            }
            if (8..=15).contains(&digits) {
                found = Some((j, "Phone", 0.8));
            }
        } else if let Some(num) = c.strip_suffix('%') {
            if is_number(num) {
                found = Some((i, "Percentage", 0.9));
            }
        } else if let Some(sym) = c.chars().next().filter(|ch| CURRENCY_SYMBOLS.contains(ch)) {
            if is_number(&c[sym.len_utf8()..]) {
                let scaled = toks
                    .get(i + 1)
                    .is_some_and(|t| SCALE_WORDS.contains(&core(t).to_lowercase().as_str()));
                found = Some((if scaled { i + 1 } else { i }, "Money", 0.9));
            }
        } else if CURRENCY_CODES.contains(&c) && toks.get(i + 1).is_some_and(|t| is_number(core(t)))
        {
            let scaled = toks
                .get(i + 2)
                .is_some_and(|t| SCALE_WORDS.contains(&core(t).to_lowercase().as_str()));
            found = Some((if scaled { i + 2 } else { i + 1 }, "Money", 0.9));
        } else if is_number(c)
            && toks
                .get(i + 1)
                .is_some_and(|t| CURRENCY_CODES.contains(&core(t)))
            // "10 EUR 150.00": the code belongs to the amount after it.
            && !toks.get(i + 2).is_some_and(|t| is_number(core(t)))
        {
            found = Some((i + 1, "Money", 0.85));
        }

        if let Some((last, kind, confidence)) = found {
            let end = toks[last].core_end;
            out.push(Match {
                name: text[start..end].to_string(),
                kind,
                start,
                end,
                confidence,
            });
            i = last + 1;
        } else {
            i += 1;
        }
    }
    out
}

// ----------------------------------------------------------------- names

const HONORIFICS: &[&str] = &["dr", "mr", "mrs", "ms", "mx", "prof", "sir", "dame"];
const PARTICLES: &[&str] = &[
    "van", "von", "de", "der", "den", "du", "da", "di", "del", "le", "la", "bin", "al", "ten",
    "ter",
];
/// Sentence-initial function words that are never part of a name.
const LEADING_STOP: &[&str] = &[
    "the", "a", "an", "this", "that", "these", "those", "our", "their", "his", "her", "its", "my",
    "your", "we", "in", "on", "at", "for", "by", "with", "from", "to", "and", "or", "but", "of",
    "as", "if", "when", "while", "after", "before", "during", "since", "until", "per", "via",
];
/// Legal forms and organisation head nouns (last-word cue).
const ORG_SUFFIXES: &[&str] = &[
    "inc",
    "corp",
    "corporation",
    "ltd",
    "limited",
    "llc",
    "llp",
    "plc",
    "gmbh",
    "ag",
    "sa",
    "sas",
    "srl",
    "spa",
    "nv",
    "n.v",
    "bv",
    "b.v",
    "kg",
    "co",
    "company",
    "group",
    "holdings",
    "industries",
    "systems",
    "technologies",
    "solutions",
    "bank",
    "partners",
    "consulting",
    "capital",
    "insurance",
    "university",
    "institute",
    "center",
    "centre",
    "foundation",
    "association",
    "agency",
    "ministry",
    "council",
    "hospital",
    "school",
    "college",
    "trading",
    "labs",
    "laboratories",
    "services",
    "enterprises",
    "ventures",
    "airlines",
    "motors",
    "pharmaceuticals",
    "media",
    "studios",
    "networks",
    "software",
    "logistics",
    "authority",
    "commission",
    "committee",
    "department",
    "society",
    "federation",
    "academy",
    "museum",
    "clinic",
];
const LOCATION_WORDS: &[&str] = &[
    "city",
    "town",
    "county",
    "state",
    "province",
    "district",
    "region",
    "street",
    "avenue",
    "road",
    "lane",
    "boulevard",
    "square",
    "harbour",
    "harbor",
    "airport",
    "island",
    "river",
    "mountain",
    "hall",
    "park",
];
/// Built-in gazetteer: countries, major cities, regions (lowercase).
const GAZETTEER: &[&str] = &[
    // countries
    "afghanistan",
    "argentina",
    "australia",
    "austria",
    "belgium",
    "brazil",
    "bulgaria",
    "canada",
    "chile",
    "china",
    "colombia",
    "croatia",
    "czechia",
    "denmark",
    "egypt",
    "estonia",
    "finland",
    "france",
    "germany",
    "greece",
    "hungary",
    "iceland",
    "india",
    "indonesia",
    "iran",
    "iraq",
    "ireland",
    "israel",
    "italy",
    "japan",
    "kenya",
    "latvia",
    "lithuania",
    "luxembourg",
    "malaysia",
    "mexico",
    "morocco",
    "netherlands",
    "new zealand",
    "nigeria",
    "norway",
    "pakistan",
    "peru",
    "philippines",
    "poland",
    "portugal",
    "romania",
    "russia",
    "saudi arabia",
    "singapore",
    "slovakia",
    "slovenia",
    "south africa",
    "south korea",
    "spain",
    "sweden",
    "switzerland",
    "thailand",
    "turkey",
    "ukraine",
    "united arab emirates",
    "united kingdom",
    "united states",
    "vietnam",
    "england",
    "scotland",
    "wales",
    // cities
    "amsterdam",
    "athens",
    "atlanta",
    "auckland",
    "bangalore",
    "bangkok",
    "barcelona",
    "beijing",
    "berlin",
    "boston",
    "brussels",
    "budapest",
    "buenos aires",
    "cairo",
    "cape town",
    "chicago",
    "copenhagen",
    "delhi",
    "new delhi",
    "dubai",
    "dublin",
    "edinburgh",
    "frankfurt",
    "geneva",
    "hamburg",
    "helsinki",
    "hong kong",
    "istanbul",
    "jakarta",
    "johannesburg",
    "lagos",
    "lisbon",
    "london",
    "los angeles",
    "madrid",
    "manchester",
    "melbourne",
    "mexico city",
    "miami",
    "milan",
    "montreal",
    "moscow",
    "mumbai",
    "munich",
    "nairobi",
    "new york",
    "osaka",
    "oslo",
    "paris",
    "prague",
    "rome",
    "rotterdam",
    "san francisco",
    "santiago",
    "sao paulo",
    "seattle",
    "seoul",
    "shanghai",
    "stockholm",
    "sydney",
    "taipei",
    "tokyo",
    "toronto",
    "utrecht",
    "vancouver",
    "vienna",
    "warsaw",
    "washington",
    "zurich",
    "the hague",
    "eindhoven",
    "antwerp",
    "lyon",
    "cologne",
    "stuttgart",
    // regions
    "europe",
    "asia",
    "africa",
    "oceania",
    "antarctica",
    "north america",
    "south america",
    "latin america",
    "central america",
    "asia pacific",
    "middle east",
    "scandinavia",
    "western europe",
    "eastern europe",
    "southeast asia",
];
/// Common given names — the cue that types an otherwise bare two-word name.
const GIVEN_NAMES: &[&str] = &[
    "aaron",
    "adam",
    "ahmed",
    "alex",
    "alexander",
    "alice",
    "amanda",
    "amy",
    "ana",
    "andrea",
    "andrew",
    "angela",
    "anna",
    "anne",
    "anthony",
    "arjun",
    "barbara",
    "ben",
    "benjamin",
    "bob",
    "brian",
    "carlos",
    "carol",
    "catherine",
    "charles",
    "chris",
    "christopher",
    "claire",
    "daniel",
    "david",
    "deborah",
    "elena",
    "elizabeth",
    "emily",
    "emma",
    "eric",
    "eva",
    "fatima",
    "frank",
    "george",
    "grace",
    "hannah",
    "hans",
    "helen",
    "henry",
    "isabel",
    "jack",
    "james",
    "jan",
    "jane",
    "jason",
    "jennifer",
    "jessica",
    "john",
    "jose",
    "joseph",
    "julia",
    "karen",
    "kevin",
    "laura",
    "linda",
    "lisa",
    "lucas",
    "luis",
    "maria",
    "mark",
    "martin",
    "mary",
    "matthew",
    "michael",
    "mohammed",
    "nancy",
    "nicole",
    "olivia",
    "omar",
    "patricia",
    "paul",
    "peter",
    "priya",
    "rachel",
    "rahul",
    "richard",
    "robert",
    "sam",
    "sarah",
    "sophie",
    "steven",
    "susan",
    "thomas",
    "tim",
    "tom",
    "victoria",
    "wei",
    "william",
    "yuki",
    "zoe",
    "bart",
    "sanne",
    "pieter",
    "lars",
    "sven",
    "ingrid",
    "klaus",
    "marco",
    "giulia",
    "pierre",
    "marie",
    "juan",
    "sofia",
    "mei",
    "li",
    "kenji",
    "raj",
    "anil",
    "neha",
];

fn bare(word: &str) -> String {
    word.trim_end_matches('.').to_lowercase()
}

fn is_name_word(core: &str) -> bool {
    core.chars().count() >= 2
        && core.chars().next().is_some_and(|c| c.is_uppercase())
        && !core.chars().any(|c| c.is_ascii_digit())
}

fn in_gazetteer(words: &[&str]) -> bool {
    let joined = words.iter().map(|w| bare(w)).collect::<Vec<_>>().join(" ");
    GAZETTEER.contains(&joined.as_str())
}

/// Cue-based type for a name (its words, punctuation-trimmed).
pub(crate) fn type_name(words: &[&str]) -> &'static str {
    if words
        .iter()
        .any(|w| ORG_SUFFIXES.contains(&bare(w).as_str()))
    {
        return "Organization";
    }
    if in_gazetteer(words)
        || words
            .iter()
            .any(|w| LOCATION_WORDS.contains(&bare(w).as_str()))
    {
        return "Location";
    }
    if (2..=3).contains(&words.len()) && GIVEN_NAMES.contains(&bare(words[0]).as_str()) {
        return "Person";
    }
    "Thing"
}

/// Mask byte ranges with spaces (offsets preserved, char-boundary safe).
fn mask(text: &str, spans: &[(usize, usize)]) -> String {
    text.char_indices()
        .map(|(i, c)| {
            if spans.iter().any(|(s, e)| i >= *s && i < *e) {
                " ".repeat(c.len_utf8())
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Named entities (capitalised phrases) with cue-based types. `skip` spans
/// (typed values, dates) are masked first so they never feed a name.
pub(crate) fn recognize_names(text: &str, skip: &[(usize, usize)]) -> Vec<Match> {
    let masked = mask(text, skip);
    let toks = tokenize(&masked);
    let core = |t: &Tok| &masked[t.core_start..t.core_end];
    let mut out: Vec<Match> = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut person_cue = false;

    let flush = |run: &mut Vec<usize>, person_cue: &mut bool, out: &mut Vec<Match>| {
        let cue = std::mem::take(person_cue);
        // Leading function words never start a name.
        while run
            .first()
            .is_some_and(|&i| LEADING_STOP.contains(&bare(core(&toks[i])).as_str()))
        {
            run.remove(0);
        }
        // A trailing particle is not part of the name.
        while run
            .last()
            .is_some_and(|&i| PARTICLES.contains(&core(&toks[i])))
        {
            run.pop();
        }
        if run.is_empty() {
            return;
        }
        let words: Vec<&str> = run.iter().map(|&i| core(&toks[i])).collect();
        let emit = |from: usize, to: usize, kind: &'static str, out: &mut Vec<Match>| {
            let (s, e) = (toks[run[from]].core_start, toks[run[to - 1]].core_end);
            let name = words[from..to].join(" ");
            if name.len() > 2 && !out.iter().any(|m| m.name == name) {
                out.push(Match {
                    name,
                    kind,
                    start: s,
                    end: e,
                    confidence: 0.85,
                });
            }
        };
        let n = words.len();
        let has_particle = words.iter().any(|w| PARTICLES.contains(w));
        let last_is_org = ORG_SUFFIXES.contains(&bare(words[n - 1]).as_str());
        if cue && n <= 4 {
            emit(0, n, "Person", out);
        } else if n == 1 {
            if in_gazetteer(&words) {
                emit(0, 1, "Location", out);
            }
        } else if last_is_org && n <= 6 {
            emit(0, n, "Organization", out);
        } else if in_gazetteer(&words) {
            emit(0, n, "Location", out);
        } else if has_particle && n <= 4 {
            emit(0, n, "Person", out);
        } else {
            // Sliding 2/3-word windows: keeps "Acme Corporation" and
            // "Annual Report" apart in "Acme Corporation Annual Report".
            for w in [2, 3] {
                for s in 0..n.saturating_sub(w - 1) {
                    let kind = type_name(&words[s..s + w]);
                    emit(s, s + w, kind, out);
                }
            }
            // Gazetteer singles inside a longer run ("Berlin Office").
            for s in 0..n {
                if in_gazetteer(&words[s..s + 1]) {
                    emit(s, s + 1, "Location", out);
                }
            }
        }
        run.clear();
    };

    for (i, t) in toks.iter().enumerate() {
        let c = core(t);
        let raw = &masked[t.start..t.end];
        if t.newline_before {
            flush(&mut run, &mut person_cue, &mut out);
        }
        if c.is_empty() {
            flush(&mut run, &mut person_cue, &mut out);
            continue;
        }
        if HONORIFICS.contains(&bare(c).as_str())
            && c.chars().next().is_some_and(char::is_uppercase)
        {
            flush(&mut run, &mut person_cue, &mut out);
            person_cue = true;
            continue;
        }
        let next_is_name = toks
            .get(i + 1)
            .is_some_and(|n| !n.newline_before && is_name_word(core(n)));
        let particle = !run.is_empty() && PARTICLES.contains(&c) && next_is_name;
        if is_name_word(c) || particle {
            run.push(i);
        } else {
            flush(&mut run, &mut person_cue, &mut out);
            continue;
        }
        // Punctuation after the word ends the run — except an abbreviation's
        // own dot ("Inc.", "B.V.").
        let tail = &raw[raw.trim_end_matches(TRAIL).len()..];
        let abbrev_dot = tail == "." && (ORG_SUFFIXES.contains(&bare(c).as_str()) || c.len() <= 3);
        if !tail.is_empty() && !abbrev_dot {
            flush(&mut run, &mut person_cue, &mut out);
        }
    }
    flush(&mut run, &mut person_cue, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(String, &'static str)> {
        recognize_values(text)
            .into_iter()
            .map(|m| (m.name, m.kind))
            .collect()
    }

    #[test]
    fn values_and_spans() {
        let t = "Pay EUR 1,500.00 (21%) to NL91 ABNA 0417 1643 00, mail a.b@x.example.";
        let v = recognize_values(t);
        let got: Vec<(&str, &str)> = v.iter().map(|m| (m.name.as_str(), m.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("EUR 1,500.00", "Money"),
                ("21%", "Percentage"),
                ("NL91 ABNA 0417 1643 00", "Iban"),
                ("a.b@x.example", "Email"),
            ]
        );
        for m in &v {
            assert_eq!(&t[m.start..m.end], m.name);
        }
    }

    #[test]
    fn non_values_are_not_matched() {
        assert!(kinds("Version 2.0 shipped in 2019 with 12 features.").is_empty());
        assert!(
            kinds("Call +1 now").is_empty(),
            "too few digits for a phone"
        );
        assert!(kinds("user@localhost is not an address").is_empty());
    }

    #[test]
    fn names_break_on_punctuation_and_drop_honorifics() {
        let n: Vec<(String, &str)> = recognize_names("Contact: Dr. Emma Clarke, London.", &[])
            .into_iter()
            .map(|m| (m.name, m.kind))
            .collect();
        assert_eq!(
            n,
            vec![
                ("Emma Clarke".to_string(), "Person"),
                ("London".to_string(), "Location")
            ]
        );
    }

    #[test]
    fn type_name_cues() {
        assert_eq!(type_name(&["Utrecht", "International"]), "Thing");
        assert_eq!(type_name(&["Acme", "Corp"]), "Organization");
        assert_eq!(type_name(&["North", "America"]), "Location");
        assert_eq!(type_name(&["Alice", "Smith"]), "Person");
    }
}
