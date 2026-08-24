use std::ops::Range;

pub mod home;
pub mod processing;
pub mod result;
pub mod settings;
pub mod tracks;
pub mod videos;

pub(crate) fn visible_range(cursor: usize, len: usize, capacity: usize) -> Range<usize> {
    if len == 0 || capacity == 0 {
        return 0..0;
    }
    let cursor = cursor.min(len - 1);
    let capacity = capacity.min(len);
    let start = cursor
        .saturating_add(1)
        .saturating_sub(capacity)
        .min(len - capacity);
    start..start + capacity
}

#[cfg(test)]
mod tests {
    use super::visible_range;

    #[test]
    fn visible_range_keeps_the_cursor_on_screen() {
        assert_eq!(visible_range(0, 0, 5), 0..0);
        assert_eq!(visible_range(0, 10, 3), 0..3);
        assert_eq!(visible_range(5, 10, 3), 3..6);
        assert_eq!(visible_range(9, 10, 3), 7..10);
        assert_eq!(visible_range(99, 10, 3), 7..10);
        assert_eq!(visible_range(2, 4, 0), 0..0);
    }
}
