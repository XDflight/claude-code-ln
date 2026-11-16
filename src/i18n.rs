//! Two-language UI text: English by default, Chinese on request.

use std::sync::atomic::{AtomicBool, Ordering};

static CHINESE: AtomicBool = AtomicBool::new(false);

pub fn zh() -> bool {
    CHINESE.load(Ordering::Relaxed)
}

pub fn set_zh(on: bool) {
    CHINESE.store(on, Ordering::Relaxed);
}

/// `t!("English", "中文")` picks the literal for the current language.
#[macro_export]
macro_rules! t {
    ($en:literal, $zh:literal) => {
        if $crate::i18n::zh() { $zh } else { $en }
    };
}

/// `tf!("English {}", "中文 {}", args…)` is `format!` in the current language.
#[macro_export]
macro_rules! tf {
    ($en:literal, $zh:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::zh() { format!($zh $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
