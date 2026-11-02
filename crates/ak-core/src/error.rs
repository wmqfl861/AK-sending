//! Error taxonomy shared by all crates, mirroring docs/PROTOCOL.zh-CN.md.
//!
//! Codes are stable on the wire; display names match the protocol list.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorCode {
    DirectUnavailable = 1,
    LocalPermissionDenied = 2,
    AuthFailed = 3,
    PeerMismatch = 4,
    RouteForbidden = 5,
    NetworkPolicyBlocked = 6,
    StorageFull = 7,
    TargetRevoked = 8,
    SourceChanged = 9,
    HashMismatch = 10,
    ProtocolUnsupported = 11,
    Cancelled = 12,
    StorageError = 13,
    Internal = 14,
}

impl ErrorCode {
    pub fn code(self) -> u16 {
        self as u16
    }

    pub fn from_code(code: u16) -> Option<Self> {
        use ErrorCode::*;
        Some(match code {
            1 => DirectUnavailable,
            2 => LocalPermissionDenied,
            3 => AuthFailed,
            4 => PeerMismatch,
            5 => RouteForbidden,
            6 => NetworkPolicyBlocked,
            7 => StorageFull,
            8 => TargetRevoked,
            9 => SourceChanged,
            10 => HashMismatch,
            11 => ProtocolUnsupported,
            12 => Cancelled,
            13 => StorageError,
            14 => Internal,
            _ => return None,
        })
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ErrorCode::DirectUnavailable => "DIRECT_UNAVAILABLE",
            ErrorCode::LocalPermissionDenied => "LOCAL_PERMISSION_DENIED",
            ErrorCode::AuthFailed => "AUTH_FAILED",
            ErrorCode::PeerMismatch => "PEER_MISMATCH",
            ErrorCode::RouteForbidden => "ROUTE_FORBIDDEN",
            ErrorCode::NetworkPolicyBlocked => "NETWORK_POLICY_BLOCKED",
            ErrorCode::StorageFull => "STORAGE_FULL",
            ErrorCode::TargetRevoked => "TARGET_REVOKED",
            ErrorCode::SourceChanged => "SOURCE_CHANGED",
            ErrorCode::HashMismatch => "HASH_MISMATCH",
            ErrorCode::ProtocolUnsupported => "PROTOCOL_UNSUPPORTED",
            ErrorCode::Cancelled => "CANCELLED",
            ErrorCode::StorageError => "STORAGE_ERROR",
            ErrorCode::Internal => "INTERNAL",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_through_every_variant() {
        for code in 1..=14u16 {
            let variant = ErrorCode::from_code(code).unwrap();
            assert_eq!(variant.code(), code);
        }
        assert_eq!(ErrorCode::from_code(0), None);
        assert_eq!(ErrorCode::from_code(15), None);
        assert_eq!(ErrorCode::from_code(u16::MAX), None);
    }

    #[test]
    fn display_uses_protocol_names() {
        assert_eq!(ErrorCode::DirectUnavailable.to_string(), "DIRECT_UNAVAILABLE");
        assert_eq!(ErrorCode::HashMismatch.to_string(), "HASH_MISMATCH");
        assert_eq!(ErrorCode::ProtocolUnsupported.to_string(), "PROTOCOL_UNSUPPORTED");
    }
}
