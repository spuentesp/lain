//! Reading optional numeric settings from the environment.

use std::str::FromStr;

/// The positive value of environment variable `name`, or `default` when it is
/// unset, blank, unparseable, or zero.
pub fn positive_env<T>(name: &str, default: T) -> T
where
    T: FromStr + PartialOrd + Default + Copy,
{
    match std::env::var(name) {
        Ok(s) if !s.trim().is_empty() => match s.trim().parse::<T>() {
            Ok(v) if v > T::default() => v,
            _ => default,
        },
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_blank_zero_and_garbage_all_fall_back_to_the_default() {
        let key = "LAIN_ENV_UTIL_TEST_VALUE";
        std::env::remove_var(key);
        assert_eq!(positive_env::<usize>(key, 7), 7);
        for bad in ["", "  ", "0", "-3", "many"] {
            std::env::set_var(key, bad);
            assert_eq!(positive_env::<usize>(key, 7), 7, "{bad:?}");
        }
        std::env::set_var(key, " 12 ");
        assert_eq!(positive_env::<usize>(key, 7), 12);
        std::env::remove_var(key);
    }
}
