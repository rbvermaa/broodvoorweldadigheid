#![forbid(unsafe_code)]

pub const TIME_ZONE: &str = "Europe/Amsterdam";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bakery_time_zone_is_amsterdam() {
        assert_eq!(TIME_ZONE, "Europe/Amsterdam");
    }
}
