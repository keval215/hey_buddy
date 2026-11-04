//! Label lexicons for agent observation (ADR 0004 §5). English-only, hand-written heuristics.
//!
//! They can ONLY cause refusals: a match marks a target `sensitive_label` or a screen
//! `payment_suspected`; a miss never allows anything the other rules refuse. They are not a
//! privacy guarantee on their own (secure fields are protected by identity, not by words).
//!
//! Matching is word-bounded on a normalised string: lowercase, every run of non-alphanumeric
//! characters becomes one space, so `"CVV/CVC:"`, `"Card-Number"` and `"card  number"` all
//! match. Phrases are stored normalised.

/// Card and bank-account data. A screen showing any of these labels is `payment_suspected`.
const PAYMENT: &[&str] = &[
    "card number",
    "cardnumber",
    "credit card",
    "debit card",
    "card no",
    "name on card",
    "cardholder",
    "card holder",
    "cvv",
    "cvv2",
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
    "passcode",
    "pass code",
    "passphrase",
    "pin",
    "pin code",
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
    "cvc",
];

/// Lowercase; every run of non-alphanumerics becomes one space; padded with a space on both
/// sides so phrases can be matched with surrounding spaces.
fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push(' ');
    let mut last_space = true;
    for c in s.chars() {
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
}
