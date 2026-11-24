use domain::Clock;
use jiff::Timestamp;

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_current_time_in_amsterdam() {
        let before = Timestamp::now();
        let now = SystemClock.now();
        let after = Timestamp::now();
        assert!(before <= now && now <= after);
        assert_eq!(
            SystemClock.now_in_bakery().time_zone().iana_name(),
            Some(domain::TIME_ZONE)
        );
    }
}
