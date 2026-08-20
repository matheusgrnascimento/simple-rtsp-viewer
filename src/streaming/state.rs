//! Connection state machine and backoff calculation (pure logic).

use std::time::Duration;

use crate::model::ConnectionStatus;

/// Default maximum backoff interval when no value is configured (Requirement
/// 6.3).
pub const DEFAULT_MAX_BACKOFF_DELAY: Duration = Duration::from_secs(30);

/// Event that causes a transition in the connection state machine (Requirements
/// 3.3, 6.1, 6.4, 6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionEvent {
    /// Decoding/connecting the stream failed (network, timeout, decoding error).
    DecodeFailure,
    /// The first video frame after a (re)connection was decoded successfully.
    FirstFrameDecoded,
    /// The disconnection was requested by the user or by the System (removal/shutdown).
    UserRequestedStop,
}

/// Pure connection state machine (Requirements 2.5, 2.6, 3.3, 6.1, 6.4, 6.6).
///
/// - `DecodeFailure` (from any state other than a requested stop) results in
///   Reconnecting: while the System is trying to (re)connect, the Camera_Tile
///   shows the "reconnecting" indicator, never the "disconnected" one. The
///   latter (Requirement 2.5) only means no reconnection attempt is in
///   progress (initial state or after a stop requested by the user).
/// - `FirstFrameDecoded` from Reconnecting or Disconnected results in Connected.
/// - `UserRequestedStop`, from any state, results in Disconnected (and does not
///   start a reconnection).
pub fn next_connection_state(
    current: ConnectionStatus,
    event: ConnectionEvent,
) -> ConnectionStatus {
    match event {
        ConnectionEvent::UserRequestedStop => ConnectionStatus::Disconnected,
        ConnectionEvent::DecodeFailure => ConnectionStatus::Reconnecting,
        ConnectionEvent::FirstFrameDecoded => match current {
            ConnectionStatus::Reconnecting | ConnectionStatus::Disconnected => {
                ConnectionStatus::Connected
            }
            ConnectionStatus::Connected => ConnectionStatus::Connected,
        },
    }
}

/// Pure backoff function (basis of the "Reconnection backoff" Correctness
/// Property).
///
/// `attempt` is the number of attempts already made (0-based). Uses exponential
/// backoff (2^attempt seconds) capped at `max_delay`, never returning less than
/// 1 second (Requirements 6.2, 6.3, 6.5). Saturating arithmetic avoids
/// overflow/panic for very large values of `attempt` (indefinite reconnection).
pub fn backoff_delay(attempt: u32, max_delay: Duration) -> Duration {
    let min_delay = Duration::from_secs(1);
    // Makes sure the maximum limit never drops below the 1s minimum.
    let max_delay = max_delay.max(min_delay);

    let exponential_secs = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let exponential_delay = Duration::from_secs(exponential_secs);

    exponential_delay.clamp(min_delay, max_delay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn status_strategy() -> impl Strategy<Value = ConnectionStatus> {
        prop_oneof![
            Just(ConnectionStatus::Connected),
            Just(ConnectionStatus::Disconnected),
            Just(ConnectionStatus::Reconnecting),
        ]
    }

    fn event_strategy() -> impl Strategy<Value = ConnectionEvent> {
        prop_oneof![
            Just(ConnectionEvent::DecodeFailure),
            Just(ConnectionEvent::FirstFrameDecoded),
            Just(ConnectionEvent::UserRequestedStop),
        ]
    }

    proptest! {
        // Feature: simple-rtsp-viewer, Property 6: Connection state machine transitions
        #[test]
        fn property_next_connection_state(current in status_strategy(), event in event_strategy()) {
            let next = next_connection_state(current, event);
            match event {
                ConnectionEvent::UserRequestedStop => {
                    prop_assert_eq!(next, ConnectionStatus::Disconnected)
                }
                ConnectionEvent::DecodeFailure => {
                    prop_assert_eq!(next, ConnectionStatus::Reconnecting)
                }
                ConnectionEvent::FirstFrameDecoded => {
                    prop_assert_eq!(next, ConnectionStatus::Connected)
                }
            }
        }

        // Feature: simple-rtsp-viewer, Property 6: UserRequestedStop never leads back to Reconnecting
        // without a new successful connection.
        #[test]
        fn property_user_stop_does_not_reconnect(current in status_strategy()) {
            let stopped = next_connection_state(current, ConnectionEvent::UserRequestedStop);
            prop_assert_eq!(stopped, ConnectionStatus::Disconnected);
        }

        // Feature: simple-rtsp-viewer, Property 7: Reconnection backoff interval calculation
        #[test]
        fn property_backoff_delay_bounds(attempt in any::<u32>(), max_secs in 0u64..120) {
            let max_delay = Duration::from_secs(max_secs);
            let d = backoff_delay(attempt, max_delay);
            let effective_max = max_delay.max(Duration::from_secs(1));
            prop_assert!(d >= Duration::from_secs(1));
            prop_assert!(d <= effective_max);
        }
    }

    #[test]
    fn backoff_is_exponential_until_cap() {
        let max = Duration::from_secs(30);
        assert_eq!(backoff_delay(0, max), Duration::from_secs(1));
        assert_eq!(backoff_delay(1, max), Duration::from_secs(2));
        assert_eq!(backoff_delay(2, max), Duration::from_secs(4));
        assert_eq!(backoff_delay(4, max), Duration::from_secs(16));
        assert_eq!(backoff_delay(5, max), Duration::from_secs(30)); // 32 limitado a 30
        assert_eq!(backoff_delay(u32::MAX, max), Duration::from_secs(30));
    }

    #[test]
    fn backoff_never_below_one_second_even_with_zero_max() {
        assert_eq!(backoff_delay(0, Duration::ZERO), Duration::from_secs(1));
    }
}
