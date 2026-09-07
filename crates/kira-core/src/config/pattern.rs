//! The restricted regular-expression dialect a config field may declare.
//!
//! `pattern` on a string field is the one part of a declaration that is neither
//! a number nor a name: it is a program, it runs on whatever the user typed, and
//! the SDK's specification is explicit that it must mean the same thing on every
//! companion app that reads it. So the dialect is a deliberately small subset —
//! no backreferences, no lookaround, no named groups, no inline flags, no
//! Unicode property escapes, no anchors — chosen so that nothing in it can
//! behave differently between JavaScript, Swift and Java. Section 3.2 of the
//! SDK's `Docs/app-config-fields.md` is the specification, and
//! `Utilities/Scripts/app_packer/validate_app_config.py` is its reference
//! implementation; [`check`] is a port of that scanner rather than an
//! independent reading of the prose, because a checker that disagreed with the
//! one the app's own CI runs would refuse manifests the SDK calls valid.
//!
//! [`matches`] then applies a pattern, which Kira has to do itself: the app on
//! the other end does not. `SDK::AppConfig` clamps numbers and truncates
//! strings, but it has no regular-expression engine and never looks at
//! `pattern`, so a value that does not match is a value the app will read and
//! act on. Refusing it is the companion app's job alone. Matching is always a
//! *full* match, so a pattern needs no anchors and is refused for carrying them.
//!
//! An engine of a few hundred lines rather than a dependency, for two reasons:
//! the subset is small enough that this is the smaller risk, and this crate
//! compiles to WebAssembly that ships to every visitor.

use std::cell::Cell;

/// Longest pattern the SDK's schema allows.
pub(crate) const MAX_PATTERN_CHARS: usize = 256;

/// Escapes that spell a letter and are allowed.
///
/// A whitelist rather than a blacklist, for the reason the SDK's scanner gives:
/// `\A` and `\Z` are anchors in Python, Java and ICU but *literal letters* in
/// JavaScript, so a pattern using one matches different strings on iOS and
/// Android. Every letter escape outside this set is refused rather than
/// classified.
const ALLOWED_LETTER_ESCAPES: &str = "dDwWsSbBnrt";

/// Punctuation a backslash may escape.
const ALLOWED_PUNCT_ESCAPES: &str = ".[](){}|+*?-/\\^$";

/// How many matching steps one value may cost before it is refused.
///
/// The dialect already refuses the shape that backtracks exponentially — a
/// quantified group whose body is itself unbounded — and a value is at most 128
/// bytes, so an ordinary pattern finishes in thousands of steps. This is the
/// backstop for the shapes a linter cannot promise to catch (`(a|a)+`, `(a?)*`),
/// where the alternative is a frozen tab: whoever wrote the pattern gets a value
/// refused, which their `validationMessage` will explain badly, and that is
/// still better than the page hanging.
const STEP_BUDGET: u32 = 200_000;

/// Refuse a pattern that is outside the dialect.
///
/// Returns the first problem, phrased for whoever wrote the manifest.
///
/// # Errors
/// If the pattern uses a construct the dialect forbids, is not well formed, or
/// carries a shape known to backtrack exponentially.
pub(crate) fn check(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("pattern is empty; omit it rather than declaring one that says nothing".into());
    }
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(format!(
            "pattern is {} characters, over the {MAX_PATTERN_CHARS} the SDK allows",
            pattern.chars().count()
        ));
    }
    check_dialect(pattern)?;
    // Well-formedness, which the scan above deliberately does not decide: it
    // walks the pattern looking for forbidden constructs and keeps going, the
    // way the SDK's does, so that one mistake does not hide the next. Parsing is
    // what stands in for the reference implementation's `re.compile`.
    parse(pattern).map(|_| ())
}

/// Whether `value` matches `pattern` in full.
///
/// `false` for a pattern that is not in the dialect: this is only ever called on
/// a declaration [`check`] has already accepted, so an unparseable one is a
/// caller's bug, and refusing the value is the safe reading of it.
#[must_use]
pub(crate) fn matches(pattern: &str, value: &str) -> bool {
    let Ok(node) = parse(pattern) else {
        return false;
    };
    let text: Vec<char> = value.chars().collect();
    let run = Matcher {
        text: &text,
        budget: Cell::new(STEP_BUDGET),
    };
    run.node(&node, 0, &mut |end| end == text.len())
}

/// Walk the pattern refusing forbidden constructs, as the SDK's scanner does.
///
/// Walks rather than pattern-matches so that a `^` inside a character class
/// (negation, legal) is not mistaken for an anchor (illegal), and so a construct
/// that follows an escape is not misread as one.
fn check_dialect(pattern: &str) -> Result<(), String> {
    let chars: Vec<char> = pattern.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut in_class = false;
    // Whether the body being scanned contains an unbounded quantifier. Stacked
    // per group, because a quantified group whose body is also unbounded is the
    // shape that backtracks exponentially.
    let mut group_stack: Vec<bool> = Vec::new();
    let mut body_unbounded = false;

    while i < n {
        let ch = chars[i];

        if ch == '\\' {
            check_escape(chars.get(i + 1).copied())?;
            i += 2;
            continue;
        }

        if in_class {
            if ch == ']' {
                in_class = false;
            }
            i += 1;
            continue;
        }

        if ch == '[' {
            in_class = true;
            i += check_class_opener(&chars, i)?;
            continue;
        }

        if ch == '^' || ch == '$' {
            return Err(format!(
                "'{ch}' is not allowed: matching is always a full match, so anchors are \
                 unnecessary"
            ));
        }

        if ch == '(' {
            let opener = check_group_opener(&chars, i, n)?;
            group_stack.push(body_unbounded);
            body_unbounded = false;
            i += opener;
            continue;
        }

        if ch == ')' {
            let Some(enclosing_unbounded) = group_stack.pop() else {
                return Err("pattern has an unbalanced ')'".into());
            };
            let inner_unbounded = body_unbounded;
            i += 1;
            let (quant, after) = read_quantifier(&chars, i);
            i = after;
            if let Some(q) = quant {
                if q.possessive {
                    // Checked on a group as well as an atom: '(a)++' is a syntax
                    // error in JavaScript, a possessive quantifier in Java and
                    // accepted by Python -- exactly the divergence this refuses.
                    return Err("possessive quantifiers are not allowed".into());
                }
                if inner_unbounded && q.unbounded {
                    return Err(
                        "a quantified group whose body is also unbounded (such as '(a+)+') can \
                         backtrack exponentially; rewrite it without the nesting"
                            .into(),
                    );
                }
            }
            // The enclosing body inherits what this group contained: a layer of
            // parentheses between an inner unbounded quantifier and an outer one
            // does not make '((a+))+' safe.
            body_unbounded =
                enclosing_unbounded || inner_unbounded || quant.is_some_and(|q| q.unbounded);
            continue;
        }

        let (quant, after) = read_quantifier(&chars, i);
        if let Some(q) = quant {
            if q.possessive {
                return Err("possessive quantifiers are not allowed".into());
            }
            if q.unbounded {
                body_unbounded = true;
            }
            i = after;
            continue;
        }

        if ch == '{' {
            // Not a well-formed {n} / {n,} / {n,m}. Python reads '{,3}' as a
            // quantifier, JavaScript as literal text, and Java throws.
            return Err(
                "'{' must open a complete {n}, {n,} or {n,m} quantifier; write a literal brace \
                 as '\\{'"
                    .into(),
            );
        }

        i += 1;
    }

    if in_class {
        return Err("pattern has an unterminated character class '['".into());
    }
    if group_stack.is_empty() {
        Ok(())
    } else {
        Err("pattern has an unbalanced '('".into())
    }
}

/// How many characters a character class's opener spends.
///
/// A `^` straight after the bracket is negation and legal. A `]` there is not:
/// Python and Java read it as a literal `]` inside the class, JavaScript as an
/// empty class followed by a literal `]`, which is the same pattern matching
/// different strings.
fn check_class_opener(chars: &[char], at: usize) -> Result<usize, String> {
    let mut spent = 1;
    if chars.get(at + spent) == Some(&'^') {
        spent += 1;
    }
    if chars.get(at + spent) == Some(&']') {
        return Err(
            "a ']' straight after '[' is a literal in some engines and an empty class in \
             others; write it as '\\]'"
                .into(),
        );
    }
    Ok(spent)
}

/// Refuse an escape the three engines do not agree about.
fn check_escape(next: Option<char>) -> Result<(), String> {
    let Some(next) = next else {
        return Err("pattern ends in a backslash".into());
    };
    if next.is_ascii_digit() && next != '0' {
        return Err(format!("backreference '\\{next}' is not allowed"));
    }
    if next == 'k' {
        return Err("named backreference '\\k' is not allowed".into());
    }
    if next == 'p' || next == 'P' {
        return Err(format!(
            "Unicode property escape '\\{next}{{...}}' is not allowed"
        ));
    }
    if next.is_alphanumeric() && !ALLOWED_LETTER_ESCAPES.contains(next) {
        return Err(format!(
            "'\\{next}' is not one of the allowed escapes ({})",
            allowed_escapes_text()
        ));
    }
    Ok(())
}

/// How many characters a group's opener spends, refusing every kind of group
/// the dialect does not have.
fn check_group_opener(chars: &[char], at: usize, n: usize) -> Result<usize, String> {
    if starts_with(chars, at, "(?:") {
        return Ok(3);
    }
    if !starts_with(chars, at, "(?") {
        return Ok(1);
    }
    let tail: String = chars[at + 2..n.min(at + 4)].iter().collect();
    Err(match tail.chars().next() {
        Some('=' | '!') => "lookahead is not allowed".into(),
        Some('<') if matches!(tail.chars().nth(1), Some('=' | '!')) => {
            "lookbehind is not allowed".into()
        }
        Some('<') => "named capture groups are not allowed".into(),
        Some('P') if tail.starts_with("P<") => "named capture groups are not allowed".into(),
        Some('>') => "atomic groups are not allowed".into(),
        _ => format!("inline group modifier '(?{tail}' is not allowed"),
    })
}

/// The allowed letter escapes, for an error message.
fn allowed_escapes_text() -> String {
    let mut sorted: Vec<char> = ALLOWED_LETTER_ESCAPES.chars().collect();
    sorted.sort_unstable();
    sorted
        .iter()
        .map(|c| format!("\\{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(k, c)| chars.get(at + k) == Some(&c))
}

/// What a quantifier at `at` is, and where it ends.
#[derive(Debug, Clone, Copy)]
struct Quantifier {
    /// `*`, `+` and `{n,}` have no upper bound.
    unbounded: bool,
    /// `a++`: allowed nowhere, because the three engines disagree about it.
    possessive: bool,
}

/// Read a quantifier at `at`, or report none.
///
/// A lazy quantifier (`a+?`) is fine — every target engine agrees on it.
fn read_quantifier(chars: &[char], at: usize) -> (Option<Quantifier>, usize) {
    let Some(&ch) = chars.get(at) else {
        return (None, at);
    };
    let (mut end, unbounded) = match ch {
        '*' | '+' => (at + 1, true),
        '?' => (at + 1, false),
        '{' => match read_braces(chars, at) {
            Some((end, unbounded)) => (end, unbounded),
            None => return (None, at),
        },
        _ => return (None, at),
    };

    let mut possessive = false;
    match chars.get(end) {
        Some('+') => {
            possessive = true;
            end += 1;
        }
        // Lazy, which is allowed.
        Some('?') => end += 1,
        _ => {}
    }
    (
        Some(Quantifier {
            unbounded,
            possessive,
        }),
        end,
    )
}

/// Parse `{n}`, `{n,}` or `{n,m}` at `at`, returning its end and whether it is
/// unbounded. `None` for anything else, including `{,3}`.
fn read_braces(chars: &[char], at: usize) -> Option<(usize, bool)> {
    let mut i = at + 1;
    let start = i;
    while chars.get(i).is_some_and(char::is_ascii_digit) {
        i += 1;
    }
    if i == start {
        return None;
    }
    match chars.get(i) {
        Some('}') => Some((i + 1, false)),
        Some(',') => {
            i += 1;
            let upper = i;
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
            if chars.get(i) == Some(&'}') {
                Some((i + 1, i == upper))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// One alternation: a choice between sequences.
#[derive(Debug)]
struct Node(Vec<Vec<Piece>>);

/// One atom with its repetition.
#[derive(Debug)]
struct Piece {
    atom: Atom,
    min: u32,
    /// `None` for `*`, `+` and `{n,}`.
    max: Option<u32>,
    lazy: bool,
}

#[derive(Debug)]
enum Atom {
    Literal(char),
    /// `.`, which in JavaScript matches anything but a line terminator.
    Any,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    Group(Box<Node>),
    /// `\b` and `\B`: zero-width, and the only assertions the dialect keeps.
    WordBoundary {
        inside: bool,
    },
}

#[derive(Debug)]
enum ClassItem {
    Char(char),
    Range(char, char),
    /// `\d`, `\w`, `\s` and their negations, usable inside a class too.
    Shorthand {
        kind: char,
        negated: bool,
    },
}

/// Parse a pattern already accepted by [`check_dialect`].
fn parse(pattern: &str) -> Result<Node, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut at = 0;
    let node = parse_alternation(&chars, &mut at)?;
    if at == chars.len() {
        Ok(node)
    } else {
        Err(format!(
            "pattern is not well formed from character {} ({:?})",
            at + 1,
            chars[at]
        ))
    }
}

fn parse_alternation(chars: &[char], at: &mut usize) -> Result<Node, String> {
    let mut branches = vec![parse_sequence(chars, at)?];
    while chars.get(*at) == Some(&'|') {
        *at += 1;
        branches.push(parse_sequence(chars, at)?);
    }
    Ok(Node(branches))
}

fn parse_sequence(chars: &[char], at: &mut usize) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    while let Some(&ch) = chars.get(*at) {
        if ch == '|' || ch == ')' {
            break;
        }
        let atom = parse_atom(chars, at)?;
        let (min, max, lazy) = parse_repetition(chars, at)?;
        pieces.push(Piece {
            atom,
            min,
            max,
            lazy,
        });
    }
    Ok(pieces)
}

/// A repetition: how few times, how many times, and whether it is lazy.
type Repetition = (u32, Option<u32>, bool);

fn parse_repetition(chars: &[char], at: &mut usize) -> Result<Repetition, String> {
    let (min, max) = match chars.get(*at) {
        Some('*') => {
            *at += 1;
            (0, None)
        }
        Some('+') => {
            *at += 1;
            (1, None)
        }
        Some('?') => {
            *at += 1;
            (0, Some(1))
        }
        Some('{') => match read_braces(chars, *at) {
            Some((end, unbounded)) => {
                let text: String = chars[*at + 1..end - 1].iter().collect();
                *at = end;
                let mut parts = text.splitn(2, ',');
                let min = parse_count(parts.next().unwrap_or_default())?;
                let max = match parts.next() {
                    None => Some(min),
                    Some("") => None,
                    Some(upper) => Some(parse_count(upper)?),
                };
                debug_assert_eq!(unbounded, max.is_none());
                if max.is_some_and(|m| m < min) {
                    return Err(format!("quantifier {{{text}}} counts down rather than up"));
                }
                (min, max)
            }
            // A brace that is not a quantifier was refused by the dialect scan,
            // so this is an escaped or literal one and belongs to the atom.
            None => return Ok((1, Some(1), false)),
        },
        _ => return Ok((1, Some(1), false)),
    };
    let lazy = chars.get(*at) == Some(&'?');
    if lazy {
        *at += 1;
    }
    Ok((min, max, lazy))
}

fn parse_count(text: &str) -> Result<u32, String> {
    text.parse::<u32>()
        .map_err(|_| format!("quantifier count {text:?} is not a number this engine can hold"))
}

fn parse_atom(chars: &[char], at: &mut usize) -> Result<Atom, String> {
    let ch = *chars
        .get(*at)
        .ok_or_else(|| "pattern ends where an expression was expected".to_owned())?;
    match ch {
        '(' => {
            *at += if starts_with(chars, *at, "(?:") { 3 } else { 1 };
            let inner = parse_alternation(chars, at)?;
            if chars.get(*at) != Some(&')') {
                return Err("pattern has an unbalanced '('".into());
            }
            *at += 1;
            Ok(Atom::Group(Box::new(inner)))
        }
        '[' => parse_class(chars, at),
        '.' => {
            *at += 1;
            Ok(Atom::Any)
        }
        '\\' => {
            *at += 1;
            let escaped = *chars
                .get(*at)
                .ok_or_else(|| "pattern ends in a backslash".to_owned())?;
            *at += 1;
            match escaped {
                'd' | 'D' | 'w' | 'W' | 's' | 'S' => Ok(Atom::Class {
                    negated: false,
                    items: vec![ClassItem::Shorthand {
                        kind: escaped.to_ascii_lowercase(),
                        negated: escaped.is_uppercase(),
                    }],
                }),
                'b' => Ok(Atom::WordBoundary { inside: true }),
                'B' => Ok(Atom::WordBoundary { inside: false }),
                'n' => Ok(Atom::Literal('\n')),
                'r' => Ok(Atom::Literal('\r')),
                't' => Ok(Atom::Literal('\t')),
                other if ALLOWED_PUNCT_ESCAPES.contains(other) => Ok(Atom::Literal(other)),
                other => Err(format!("'\\{other}' is not an escape the dialect allows")),
            }
        }
        ')' | '|' => Err("pattern has an empty expression".into()),
        '*' | '+' | '?' => Err(format!("'{ch}' has nothing to repeat")),
        other => {
            *at += 1;
            Ok(Atom::Literal(other))
        }
    }
}

fn parse_class(chars: &[char], at: &mut usize) -> Result<Atom, String> {
    *at += 1; // '['
    let negated = chars.get(*at) == Some(&'^');
    if negated {
        *at += 1;
    }
    let mut items: Vec<ClassItem> = Vec::new();
    loop {
        let ch = *chars
            .get(*at)
            .ok_or_else(|| "pattern has an unterminated character class '['".to_owned())?;
        if ch == ']' {
            *at += 1;
            return Ok(Atom::Class { negated, items });
        }
        let low = parse_class_member(chars, at)?;
        // A '-' before the closing bracket is a literal dash, not a range.
        if chars.get(*at) == Some(&'-') && chars.get(*at + 1).is_some_and(|c| *c != ']') {
            *at += 1;
            let high = parse_class_member(chars, at)?;
            match (low, high) {
                (ClassItem::Char(a), ClassItem::Char(b)) if a <= b => {
                    items.push(ClassItem::Range(a, b));
                }
                (ClassItem::Char(a), ClassItem::Char(b)) => {
                    return Err(format!(
                        "character range {a:?}-{b:?} counts down rather than up"
                    ));
                }
                _ => {
                    return Err(
                        "a character range cannot have a class shorthand as one of its ends".into(),
                    );
                }
            }
        } else {
            items.push(low);
        }
    }
}

fn parse_class_member(chars: &[char], at: &mut usize) -> Result<ClassItem, String> {
    let ch = *chars
        .get(*at)
        .ok_or_else(|| "pattern has an unterminated character class '['".to_owned())?;
    if ch != '\\' {
        *at += 1;
        return Ok(ClassItem::Char(ch));
    }
    *at += 1;
    let escaped = *chars
        .get(*at)
        .ok_or_else(|| "pattern ends in a backslash".to_owned())?;
    *at += 1;
    match escaped {
        'd' | 'D' | 'w' | 'W' | 's' | 'S' => Ok(ClassItem::Shorthand {
            kind: escaped.to_ascii_lowercase(),
            negated: escaped.is_uppercase(),
        }),
        'n' => Ok(ClassItem::Char('\n')),
        'r' => Ok(ClassItem::Char('\r')),
        't' => Ok(ClassItem::Char('\t')),
        // \b inside a class is a backspace in every one of the three engines,
        // and a value carrying one is refused long before it gets here.
        'b' => Ok(ClassItem::Char('\u{8}')),
        other if ALLOWED_PUNCT_ESCAPES.contains(other) => Ok(ClassItem::Char(other)),
        other => Err(format!(
            "'\\{other}' is not an escape the dialect allows inside a character class"
        )),
    }
}

/// JavaScript's `\w`.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// JavaScript's `\s`: Unicode whitespace, plus the byte-order mark that
/// JavaScript alone counts as space.
fn is_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// JavaScript's `.`, which stops at a line terminator.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn class_item_matches(item: &ClassItem, c: char) -> bool {
    match item {
        ClassItem::Char(want) => *want == c,
        ClassItem::Range(low, high) => *low <= c && c <= *high,
        ClassItem::Shorthand { kind, negated } => {
            let hit = match kind {
                'd' => c.is_ascii_digit(),
                'w' => is_word(c),
                _ => is_space(c),
            };
            hit != *negated
        }
    }
}

/// One run of the matcher over one value.
///
/// A struct rather than free functions with a `&mut u32`: the continuations
/// below nest, so a budget passed as a mutable reference would be borrowed twice
/// at once. A [`Cell`] is the whole reason this holds state at all.
struct Matcher<'a> {
    text: &'a [char],
    budget: Cell<u32>,
}

impl Matcher<'_> {
    /// Spend one step, or report that there are none left.
    fn step(&self) -> bool {
        let left = self.budget.get();
        if left == 0 {
            return false;
        }
        self.budget.set(left - 1);
        true
    }

    /// Whether an alternation matches at `at`, calling `k` with where it ended.
    fn node(&self, node: &Node, at: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        node.0.iter().any(|branch| self.sequence(branch, at, k))
    }

    fn sequence(&self, pieces: &[Piece], at: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        match pieces.split_first() {
            None => k(at),
            Some((first, rest)) => {
                self.piece(first, at, 0, &mut |next| self.sequence(rest, next, k))
            }
        }
    }

    /// Whether `piece` matches from `at`, having already matched `done` times.
    ///
    /// Greedy by default: more repetitions are tried before fewer, and a lazy
    /// quantifier reverses that. A repetition that consumed nothing is not tried
    /// again once the minimum is met, which is what stops a zero-width body
    /// under a `*` — `(a?)*` — from looping forever.
    fn piece(&self, piece: &Piece, at: usize, done: u32, k: &mut dyn FnMut(usize) -> bool) -> bool {
        let may_repeat = piece.max.is_none_or(|max| done < max);
        let may_stop = done >= piece.min;

        if piece.lazy {
            if may_stop && k(at) {
                return true;
            }
            if may_repeat
                && self.atom(&piece.atom, at, &mut |next| {
                    next != at && self.piece(piece, next, done + 1, k)
                })
            {
                return true;
            }
            return false;
        }

        if may_repeat
            && self.atom(&piece.atom, at, &mut |next| {
                if next == at && done >= piece.min {
                    return false;
                }
                self.piece(piece, next, done + 1, k)
            })
        {
            return true;
        }
        may_stop && k(at)
    }

    /// Whether one atom matches at `at`.
    fn atom(&self, atom: &Atom, at: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
        if !self.step() {
            return false;
        }
        match atom {
            Atom::Group(inner) => self.node(inner, at, k),
            Atom::WordBoundary { inside } => {
                let before = at > 0 && is_word(self.text[at - 1]);
                let after = self.text.get(at).copied().is_some_and(is_word);
                if (before != after) == *inside {
                    k(at)
                } else {
                    false
                }
            }
            Atom::Literal(want) => self.text.get(at) == Some(want) && k(at + 1),
            Atom::Any => self
                .text
                .get(at)
                .copied()
                .is_some_and(|c| !is_line_terminator(c) && k(at + 1)),
            Atom::Class { negated, items } => self.text.get(at).copied().is_some_and(|c| {
                let hit = items.iter().any(|item| class_item_matches(item, c));
                hit != *negated && k(at + 1)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialect_accepts_what_the_apps_in_the_catalogue_declare() {
        // Verbatim from watch-apps: Barcode's id, name and format fields.
        for good in [
            "[ -~]{0,22}",
            "[ -~]{0,12}",
            "(?:[Cc][Oo][Dd][Ee]128|[Qq][Rr][Cc][Oo][Dd][Ee]|[Ii][Tt][Ff])?",
            "[A-Za-z0-9 ]+",
            "\\d{1,3}\\.\\d+",
            "[^0-9]*",
            "a{2,}",
            "(a|b)+",
            "x*?",
            "\\[\\]",
            "[\\]]",
        ] {
            assert!(check(good).is_ok(), "refused {good:?}: {:?}", check(good));
        }
    }

    #[test]
    fn a_construct_the_three_engines_disagree_about_is_refused() {
        for (bad, why) in [
            ("^abc$", "anchors"),
            ("(a)\\1", "backreference"),
            ("(?=a)b", "lookahead"),
            ("(?<=a)b", "lookbehind"),
            ("(?<name>a)", "named capture"),
            ("(?i)abc", "inline flag"),
            ("(?>a)", "atomic group"),
            ("\\p{L}+", "property escape"),
            ("\\Aabc", "\\A is an anchor in some engines"),
            ("a++", "possessive"),
            ("(a)++", "possessive on a group"),
            ("[]]", "']' straight after '['"),
            ("a{,3}", "incomplete quantifier"),
            ("(a+)+", "nested unbounded"),
            ("((a+))+", "nested unbounded behind parentheses"),
            ("(\\d+)*", "nested unbounded"),
            ("abc\\", "trailing backslash"),
            ("[abc", "unterminated class"),
            ("(abc", "unbalanced ("),
            ("abc)", "unbalanced )"),
        ] {
            assert!(check(bad).is_err(), "accepted {bad:?} ({why})");
        }
    }

    #[test]
    fn a_pattern_over_the_length_limit_is_refused() {
        assert!(check(&"a".repeat(MAX_PATTERN_CHARS)).is_ok());
        assert!(check(&"a".repeat(MAX_PATTERN_CHARS + 1)).is_err());
    }

    #[test]
    fn matching_is_a_full_match_without_being_asked() {
        assert!(matches("[A-Z]+", "ABC"));
        assert!(!matches("[A-Z]+", "ABC1"));
        assert!(!matches("[A-Z]+", "1ABC"));
        // A top-level alternation binds as the whole pattern, which is what the
        // SDK's JavaScript recipe wraps in (?:...) to guarantee.
        assert!(matches("abc|def", "def"));
        assert!(!matches("abc|def", "abcx"));
        assert!(!matches("abc|def", "xdef"));
    }

    #[test]
    fn the_formats_barcode_accepts_are_the_ones_that_match() {
        let fmt = "(?:[Cc][Oo][Dd][Ee]128|[Qq][Rr][Cc][Oo][Dd][Ee]|[Ii][Tt][Ff])?";
        for good in ["Code128", "code128", "QRCode", "ITF", "itf", ""] {
            assert!(matches(fmt, good), "refused {good:?}");
        }
        for bad in ["Code129", "QR", "Code128 ", "ITFX"] {
            assert!(!matches(fmt, bad), "accepted {bad:?}");
        }
    }

    #[test]
    fn quantifiers_count_the_way_they_read() {
        assert!(matches("a{2,3}", "aa"));
        assert!(matches("a{2,3}", "aaa"));
        assert!(!matches("a{2,3}", "a"));
        assert!(!matches("a{2,3}", "aaaa"));
        assert!(matches("a{2,}", "aaaaa"));
        assert!(matches("[ -~]{0,22}", ""));
        assert!(matches("[ -~]{0,22}", "A1234567"));
        assert!(!matches("[ -~]{0,22}", &"x".repeat(23)));
    }

    #[test]
    fn a_greedy_repetition_gives_back_what_the_rest_needs() {
        // The classic case for backtracking: .* has to yield the final digit.
        assert!(matches(".*\\d", "abc7"));
        assert!(matches("[a-z]*[a-z]", "abc"));
        assert!(!matches(".*\\d", "abc"));
    }

    #[test]
    fn a_lazy_repetition_matches_as_little_as_it_can_and_still_completes() {
        assert!(matches("a+?b", "aaab"));
        assert!(matches("a*?", ""));
    }

    #[test]
    fn shorthand_classes_mean_what_javascript_means_by_them() {
        assert!(matches("\\d+", "0123"));
        assert!(!matches("\\d+", "12a"));
        assert!(matches("\\w+", "a_1"));
        assert!(!matches("\\w+", "a-1"));
        assert!(matches("\\s", " "));
        assert!(matches("[\\d.]+", "1.25"));
        assert!(matches("\\S+", "x"));
        assert!(!matches("\\S+", " "));
        assert!(matches("[^0-9]+", "abc"));
        assert!(!matches("[^0-9]+", "ab1"));
    }

    #[test]
    fn a_word_boundary_is_zero_width() {
        assert!(matches("\\bword\\b", "word"));
        // Between two word characters there is no boundary, which is what \B
        // asserts; at the start of "ord" there is one, so \B fails there.
        assert!(matches("or\\Bd", "ord"));
        assert!(!matches("\\Bord", "ord"));
    }

    #[test]
    fn a_dot_does_not_cross_a_line_terminator() {
        assert!(matches(".", "x"));
        assert!(!matches(".", "\n"));
    }

    #[test]
    fn an_escaped_metacharacter_is_a_literal() {
        assert!(matches("\\.", "."));
        assert!(!matches("\\.", "x"));
        assert!(matches("a\\+b", "a+b"));
        assert!(matches("[\\]]", "]"));
        assert!(matches("\\{3\\}", "{3}"));
    }

    #[test]
    fn a_zero_width_body_under_a_star_terminates() {
        // '(a?)*' is one of the shapes the dialect cannot refuse and a naive
        // matcher loops on forever. It has to finish, whatever it answers.
        assert!(matches("(a?)*", "aaa"));
        assert!(matches("(a?)*", ""));
        assert!(!matches("(a?)*", "b"));
    }

    #[test]
    fn a_pattern_that_backtracks_without_bound_gives_up_instead_of_hanging() {
        // '(a|a)+' is accepted by the dialect -- no nesting, nothing forbidden --
        // and is exponential on a non-matching tail. The budget is what keeps a
        // 128-byte value from freezing the page.
        let value = format!("{}b", "a".repeat(40));
        assert!(!matches("(a|a)+", &value));
    }

    #[test]
    fn a_value_with_no_pattern_of_its_own_is_matched_literally() {
        assert!(matches("Code128", "Code128"));
        assert!(!matches("Code128", "code128"));
    }
}
