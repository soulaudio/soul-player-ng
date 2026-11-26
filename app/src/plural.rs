//! Plural forms for the shipped locales. `rust-i18n` has no plural support, so a
//! counted string is a key FAMILY in `locales/app.yml`: `<base>.one`,
//! `<base>.few`, `<base>.other`, each locale defining exactly the CLDR cardinal
//! categories it uses for integers (see [`required_categories`]); [`plural_key`]
//! picks the member for a count in the current locale.
#![forbid(unsafe_code)]

/// A CLDR cardinal plural category (the subset the shipped locales use for
/// whole numbers; Czech `many` only applies to decimals).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluralCategory {
    One,
    Few,
    Other,
}

impl PluralCategory {
    /// The key suffix of this category's family member (`one`, `few`, `other`).
    pub(crate) fn suffix(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Few => "few",
            Self::Other => "other",
        }
    }
}

/// The CLDR cardinal category of the whole number `n` in `locale`: cs
/// one (1) / few (2-4) / other; zh and ja always other; en, de, es (and any
/// other locale) one (1) / other.
pub(crate) fn plural_category(locale: &str, n: usize) -> PluralCategory {
    use PluralCategory::{Few, One, Other};
    match locale {
        "cs" => match n {
            1 => One,
            2..=4 => Few,
            _ => Other,
        },
        "zh" | "ja" => Other,
        _ => {
            if n == 1 {
                One
            } else {
                Other
            }
        }
    }
}

/// The categories every plural family must define for `locale` (enforced by
/// `i18n_tests::plural_families_define_exactly_each_locales_categories`).
/// Test-only: no runtime code needs the list.
#[cfg(test)]
pub(crate) fn required_categories(locale: &str) -> &'static [PluralCategory] {
    use PluralCategory::{Few, One, Other};
    match locale {
        "cs" => &[One, Few, Other],
        "zh" | "ja" => &[Other],
        _ => &[One, Other],
    }
}

/// The family member of `base` for count `n` in the current UI locale.
pub(crate) fn plural_key(base: &str, n: usize) -> String {
    format!(
        "{base}.{}",
        plural_category(&rust_i18n::locale(), n).suffix()
    )
}

/// `base`'s member for `n`, translated with `%{n}` = `n`.
pub(crate) fn tn(base: &str, n: usize) -> String {
    t!(plural_key(base, n), n => n).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use PluralCategory::{Few, One, Other};

    #[test]
    fn english_german_spanish_have_one_and_other() {
        for loc in ["en", "de", "es"] {
            assert_eq!(plural_category(loc, 1), One, "{loc}");
            for n in [0, 2, 5, 21, 101] {
                assert_eq!(plural_category(loc, n), Other, "{loc} {n}");
            }
        }
    }

    #[test]
    fn czech_has_one_few_and_other() {
        assert_eq!(plural_category("cs", 1), One);
        for n in [2, 3, 4] {
            assert_eq!(plural_category("cs", n), Few, "{n}");
        }
        for n in [0, 5, 11, 21, 22, 100] {
            assert_eq!(plural_category("cs", n), Other, "{n}");
        }
    }

    #[test]
    fn chinese_and_japanese_have_no_plural() {
        for loc in ["zh", "ja"] {
            for n in [0, 1, 2, 5] {
                assert_eq!(plural_category(loc, n), Other, "{loc} {n}");
            }
        }
    }

    #[test]
    fn every_category_a_locale_picks_is_one_it_requires() {
        for &loc in crate::LOCALES {
            for n in 0..200 {
                assert!(
                    required_categories(loc).contains(&plural_category(loc, n)),
                    "{loc} {n}"
                );
            }
        }
    }

    #[test]
    fn plural_key_uses_the_current_locale() {
        let _locale = crate::config::use_test_locale("cs");
        assert_eq!(
            plural_key("settings.organizePruned", 3),
            "settings.organizePruned.few"
        );
        rust_i18n::set_locale("en");
        assert_eq!(
            plural_key("settings.organizePruned", 3),
            "settings.organizePruned.other"
        );
        assert_eq!(tn("settings.organizePruned", 1), "1 emptied folder removed");
    }
}
