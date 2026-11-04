//! Label lexicons for agent observation (ADR 0004 §5). English-only, hand-written heuristics.
//!
//! They can ONLY cause refusals: a match marks a target `sensitive_label` or a screen
//! `payment_suspected`; a miss never allows anything the other rules refuse. They are not a
//! privacy guarantee on their own (secure fields are protected by identity, not by words).
//!
//! Matching is word-bounded on a normalised string: invisible / format characters (Cf, bidi,
//! zero-width, tags, ...) are STRIPPED first (not turned into spaces), so `Pass<ZWSP>word` is one
//! word; then lowercase, and every run of non-alphanumeric characters becomes one space, so
//! `"CVV/CVC:"`, `"Card-Number"` and `"card  number"` all match. Phrases are stored normalised.
//!
//! The destructive / purchase / submit lexicons (S2-14e) are evaluated on the JOINED label here,
//! in the firewall, and exposed read-only through `Observation::describe_flags`. `policy` keeps
//! its own copy of the lists as defence in depth and ORs both (stricter only).

/// Card and bank-account data. A screen showing any of these labels is `payment_suspected`.
const PAYMENT: &[&str] = &[
    "card number",
    "cardnumber",
    "credit card",
    "debit card",
    "card no",
    "card num",
    "name on card",
    "cardholder",
    "card holder",
    "cvv",
    "cvv2",
    "ccv",
    "cvc",
    "cvc2",
    "cid",
    "security code",
    "card security",
    "expiry",
    "expiry date",
    "expiration",
    "expiration date",
    "expire date",
    "exp date",
    "valid thru",
    "valid through",
    "iban",
    "bic",
    "swift code",
    "sort code",
    "routing number",
    "account number",
    "upi pin",
];

/// Secrets and one-time codes. A text target with such a label is `sensitive_label`.
const SECRET: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passwort",
    "passcode",
    "pass code",
    "passphrase",
    "secret",
    "pin",
    "pin code",
    "pin number",
    "otp",
    "one time code",
    "one time password",
    "one time passcode",
    "verification code",
    "security code",
    "auth code",
    "authentication code",
    "authenticator code",
    "2fa",
    "two factor",
    "two step",
    "2 step",
    "mfa",
    "recovery code",
    "backup code",
    "seed phrase",
    "recovery phrase",
    "secret phrase",
    "mnemonic",
    "api key",
    "apikey",
    "secret key",
    "private key",
    "access key",
    "token",
    "access token",
    "auth token",
    "bearer",
    "ssn",
    "social security",
    "cvv",
    "ccv",
    "cvc",
    "card num",
];

/// Destructive verbs (a click on such a label is refused by policy; S2-14e).
const DESTRUCTIVE: &[&str] = &[
    "delete",
    "remove",
    "erase",
    "uninstall",
    "format",
    "reset",
    "wipe",
    "empty",
    "close account",
    "discard",
];

/// Purchase / money-moving verbs.
const PURCHASE: &[&str] = &[
    "buy",
    "pay",
    "checkout",
    "check out",
    "place order",
    "subscribe",
    "donate",
    "transfer",
    "send money",
    "top up",
];

/// Submit-like verbs (escalate a click to at least Consequential).
const SUBMIT: &[&str] = &[
    "submit", "send", "post", "publish", "reply", "save", "sign in", "signin", "confirm", "ok",
    "continue", "next", "apply",
];

/// Lowercase; every run of non-alphanumerics becomes one space; padded with a space on both
/// sides so phrases can be matched with surrounding spaces.
fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(' ');
    let mut last_space = true;
    // Invisible / format characters vanish (they must not split a word); whitespace-like
    // controls (`\n`, `\t`, U+2028) stay separators.
    for c in s.chars().filter(|&c| !is_invisible(c)) {
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                out.push(l);
            }
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    if !last_space {
        out.push(' ');
    }
    out
}

/// Format / invisible characters: hostile but not whitespace.
fn is_invisible(c: char) -> bool {
    crate::text::is_hostile_char(c) && !c.is_whitespace()
}

fn matches_any(text: &str, phrases: &[&str]) -> bool {
    let n = normalise(text);
    phrases.iter().any(|p| n.contains(&format!(" {p} ")))
}

/// The text looks like a card / bank-account field or payment form label.
pub(crate) fn payment_suspected(text: &str) -> bool {
    matches_any(text, PAYMENT)
}

/// The label of a text target names a secret or a one-time code.
pub(crate) fn sensitive_label(text: &str) -> bool {
    matches_any(text, SECRET)
}

/// The (joined, Cf-stripped) label names a destructive action.
pub(crate) fn destructive_label(text: &str) -> bool {
    matches_any(text, DESTRUCTIVE)
}

/// The label names a purchase / payment action.
pub(crate) fn purchase_label(text: &str) -> bool {
    matches_any(text, PURCHASE)
}

/// The label names a submit-like action.
pub(crate) fn submit_label(text: &str) -> bool {
    matches_any(text, SUBMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_phrases_match_in_many_spellings() {
        for s in [
            "Card number",
            "CARD-NUMBER",
            "Credit card",
            "Debit  Card",
            "CVV",
            "CVC/CVV:",
            "Security code",
            "Expiry date (MM/YY)",
            "Expiration Date",
            "IBAN",
            "Sort code",
            "Routing number",
            "UPI PIN",
            "Enter your card number to continue",
        ] {
            assert!(payment_suspected(s), "{s}");
        }
    }

    #[test]
    fn payment_needs_whole_words() {
        for s in [
            "Cidade",
            "Rabbit",
            "Ibanez guitars",
            "",
            "Search",
            "Sign in",
        ] {
            assert!(!payment_suspected(s), "{s}");
        }
    }

    #[test]
    fn secret_phrases_match() {
        for s in [
            "Password",
            "Confirm password",
            "Passcode",
            "PIN",
            "Enter your PIN:",
            "OTP",
            "One-time code",
            "Verification code",
            "2FA code",
            "Recovery code",
            "Seed phrase",
            "Recovery phrase",
            "API key",
            "Access token",
            "SSN",
            "Social Security number",
        ] {
            assert!(sensitive_label(s), "{s}");
        }
    }

    #[test]
    fn secret_needs_whole_words_and_ordinary_labels_pass() {
        for s in [
            "Spinning",
            "Pinterest",
            "Tokens of appreciation x",
            "Email",
            "Search",
            "Sign in",
            "",
        ] {
            // "Tokens" is a different word from "token": whole words only.
            assert!(!sensitive_label(s), "{s}");
        }
    }

    #[test]
    fn normalise_collapses_and_pads() {
        assert_eq!(normalise("  A--b__C "), " a b c ");
        assert_eq!(normalise(""), " ");
    }

    #[test]
    fn format_characters_are_stripped_not_spaced() {
        for s in [
            "Pass\u{200B}word",
            "Pass\u{200D}word",
            "P\u{200B}I\u{200B}N",
            "O\u{2060}TP",
            "\u{FEFF}Password",
            "Pass\u{00AD}word",
            "Pass\u{E0041}word",
            "One\u{200B}-time code",
        ] {
            assert!(sensitive_label(s), "{s:?}");
        }
        for s in [
            "C\u{200B}VV",
            "Card\u{200B} number",
            "Card\u{200B}number",
            "IB\u{200C}AN",
        ] {
            assert!(payment_suspected(s), "{s:?}");
        }
        // Whitespace-like controls still separate words.
        assert!(!sensitive_label("Pass\nword"));
        assert!(sensitive_label("Enter\nPIN\n"));
        assert_eq!(normalise("a\u{200B}b"), " ab ");
    }

    #[test]
    fn abbreviations() {
        for s in ["Pwd", "Passwort", "Secret", "PIN number", "CCV", "Card num"] {
            assert!(sensitive_label(s), "{s}");
        }
        for s in ["CCV", "Card num", "Card Num."] {
            assert!(payment_suspected(s), "{s}");
        }
        assert!(!sensitive_label("Secretary"));
        assert!(!sensitive_label("Pwdx"));
    }

    #[test]
    fn action_lexicons_on_joined_labels() {
        for s in ["Del\u{200B}ete", "DELETE", "Remove item", "Wipe-disk"] {
            assert!(destructive_label(s), "{s:?}");
        }
        for s in [
            "Buy\u{200D}",
            "Buy now",
            "Check out",
            "Pay",
            "Place\u{200B} order",
        ] {
            assert!(purchase_label(s), "{s:?}");
        }
        for s in ["Sub\u{200B}mit", "Send", "Sign in", "OK"] {
            assert!(submit_label(s), "{s:?}");
        }
        for s in ["Deleted items", "Payment", "Booking", ""] {
            assert!(!destructive_label(s) && !purchase_label(s), "{s:?}");
        }
        assert!(!submit_label("Okay then"));
    }
}
