//! The gate fixture: the smallest crate every gate passes.

/// The sum of `a` and `b`.
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::add;

    #[test]
    fn one_and_two_make_three() {
        assert_eq!(add(1, 2), 3);
    }
}
