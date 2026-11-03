//! Friendly Chinese user copy for `send` failures (UX.zh-CN error table).
//!
//! The mapping never replaces the original error: callers attach the hint as
//! an anyhow context so the full error chain (hint -> EngineError -> causes)
//! stays visible in the final report. Probes and other verbs keep the plain
//! engine wording.
#![forbid(unsafe_code)]

use ak_core::error::ErrorCode;
use ak_engine::EngineError;

use crate::human_bytes;

/// One-line Chinese advice per wire error code. Exhaustive over
/// [`ErrorCode`] on purpose so a new code fails compilation here instead of
/// silently losing its advice.
pub(crate) fn rejection_advice(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::DirectUnavailable => {
            "对端报告当前没有可用的直连路径；确认两机在同一网络，或改用公网 probe 流程"
        }
        ErrorCode::LocalPermissionDenied => {
            "对端本地权限不足（接收目录不可写或存储授权被拒）；请对端检查目录权限后重试"
        }
        ErrorCode::AuthFailed => "对端身份认证失败；请核对两端的指纹与配对码",
        ErrorCode::PeerMismatch => {
            "对端收到的配对信息与本机身份不一致；请在两端重新核对指纹（ak-cli pair code）"
        }
        ErrorCode::RouteForbidden => "当前网络路径被策略拒绝（局域网策略下不走公网）",
        ErrorCode::NetworkPolicyBlocked => "网络策略阻止了该路径；请在允许点对点直连的网络下重试",
        ErrorCode::StorageFull => "对端磁盘空间不足；请对端清理后重试",
        ErrorCode::TargetRevoked => "对端已撤销本次接收授权；请对端重新发起接收",
        ErrorCode::SourceChanged => {
            "传输期间源文件发生了变化；请勿在传输中改动源目录，确认后重跑同一命令"
        }
        ErrorCode::HashMismatch => "数据校验不一致（哈希不匹配）；请重试；若持续出现请检查两端磁盘",
        ErrorCode::ProtocolUnsupported => {
            "两端协议版本不一致；请把两端 ak-cli 升级到同一版本后重试"
        }
        ErrorCode::Cancelled => "传输被取消",
        ErrorCode::StorageError => "对端存储读写出错；请对端检查磁盘与接收目录状态",
        ErrorCode::Internal => "对端内部错误；请对端查看日志后重试",
    }
}

/// Friendly send-side hint for an engine error, when one exists. `None`
/// means "no dedicated copy: show the engine wording unchanged".
///
/// Covered: destination-full with the real numbers, every rejection code
/// (including policy route refusals arriving either as a local policy gate
/// or as a wire error), and the idle timeout with its resume promise.
pub(crate) fn friendly_send_hint(error: &EngineError) -> Option<String> {
    match error {
        EngineError::StorageFull { available, needed } => Some(format!(
            "对端磁盘空间不足（需 {}，可用 {}）；请对端清理后重试",
            human_bytes(*needed),
            human_bytes(*available)
        )),
        EngineError::Rejected(code) => Some(format!(
            "对端拒绝了本次发送（{code}）：{}",
            rejection_advice(*code)
        )),
        EngineError::PeerError { code, message } => Some(format!(
            "对端返回错误 {code}（{message}）：{}",
            rejection_advice(*code)
        )),
        EngineError::RouteForbidden(_path) => {
            Some("当前网络路径被策略拒绝（局域网策略下不走公网）".to_owned())
        }
        EngineError::IdleTimeout(idle) => Some(format!(
            "对端长时间无响应（空闲超时 {}s）；断点已保留，网络恢复后重跑同一命令即可续传",
            idle.as_secs()
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn storage_full_hint_carries_both_sizes() {
        let hint = friendly_send_hint(&EngineError::StorageFull {
            available: 1024,
            needed: 4 * 1024 * 1024,
        })
        .expect("StorageFull must map");
        assert!(hint.contains("对端磁盘空间不足"), "{hint}");
        assert!(hint.contains("4.00 MiB"), "{hint}"); // needed
        assert!(hint.contains("1.00 KiB"), "{hint}"); // available
        assert!(hint.contains("需 4.00 MiB，可用 1.00 KiB"), "{hint}");
        assert!(hint.contains("请对端清理后重试"), "{hint}");
    }

    #[test]
    fn rejected_codes_each_get_specific_advice() {
        let hint = |code| {
            friendly_send_hint(&EngineError::Rejected(code))
                .unwrap_or_else(|| panic!("{code} must map"))
        };
        // Pairing mismatch: check the pairing information.
        assert!(
            hint(ErrorCode::PeerMismatch).contains("配对"),
            "PeerMismatch"
        );
        // Protocol version skew: upgrade both ends.
        assert!(
            hint(ErrorCode::ProtocolUnsupported).contains("版本"),
            "ProtocolUnsupported"
        );
        assert!(
            hint(ErrorCode::ProtocolUnsupported).contains("升级"),
            "ProtocolUnsupported"
        );
        // Every code keeps its wire name so users can quote it.
        assert!(hint(ErrorCode::PeerMismatch).contains("PEER_MISMATCH"));
        assert!(hint(ErrorCode::ProtocolUnsupported).contains("PROTOCOL_UNSUPPORTED"));
        // Destination-side storage full coming back as a rejection.
        assert!(hint(ErrorCode::StorageFull).contains("清理"), "StorageFull");
        // Auth and revocation have their own advice, not a shared fallback.
        assert!(hint(ErrorCode::AuthFailed).contains("指纹"), "AuthFailed");
        assert!(
            hint(ErrorCode::TargetRevoked).contains("重新发起接收"),
            "TargetRevoked"
        );
    }

    #[test]
    fn every_error_code_has_nonempty_advice() {
        for code in 1..=14u16 {
            let code = ErrorCode::from_code(code).unwrap_or_else(|| panic!("code {code}"));
            let advice = rejection_advice(code);
            assert!(!advice.is_empty(), "{code} has no advice");
            assert!(!advice.contains("TODO"), "{code} kept a placeholder");
        }
    }

    #[test]
    fn route_forbidden_maps_from_all_three_shapes() {
        // Local policy gate (the EngineError variant added with DirectPolicy).
        let local = friendly_send_hint(&EngineError::RouteForbidden(
            ak_core::policy::PathKind::InternetDirect,
        ))
        .expect("RouteForbidden variant must map");
        assert!(local.contains("当前网络路径被策略拒绝"), "{local}");
        assert!(local.contains("局域网策略下不走公网"), "{local}");

        // Same decision reported by the peer over the wire.
        let rejected = friendly_send_hint(&EngineError::Rejected(ErrorCode::RouteForbidden))
            .expect("Rejected(RouteForbidden) must map");
        assert!(rejected.contains("当前网络路径被策略拒绝"), "{rejected}");

        let peer_error = friendly_send_hint(&EngineError::PeerError {
            code: ErrorCode::RouteForbidden,
            message: "lan-only policy refuses internet path".into(),
        })
        .expect("PeerError(RouteForbidden) must map");
        assert!(
            peer_error.contains("当前网络路径被策略拒绝"),
            "{peer_error}"
        );
        assert!(peer_error.contains("ROUTE_FORBIDDEN"), "{peer_error}");
        assert!(peer_error.contains("lan-only policy"), "{peer_error}");
    }

    #[test]
    fn idle_timeout_hint_promises_resume() {
        let hint = friendly_send_hint(&EngineError::IdleTimeout(Duration::from_secs(30)))
            .expect("IdleTimeout must map");
        assert!(hint.contains("空闲超时 30s"), "{hint}");
        assert!(hint.contains("对端长时间无响应"), "{hint}");
        assert!(hint.contains("断点已保留"), "{hint}");
        assert!(hint.contains("重跑同一命令即可续传"), "{hint}");
    }

    #[test]
    fn other_engine_errors_pass_through_unchanged() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        for error in [
            EngineError::Connect(ak_net::ConnectError::Timeout(Duration::from_secs(5))),
            EngineError::Connect(ak_net::ConnectError::Unauthorized),
            EngineError::Io(io),
            EngineError::PeerClosed,
            EngineError::SourceChanged,
            EngineError::VerificationFailed,
            EngineError::Cancelled,
        ] {
            assert!(
                friendly_send_hint(&error).is_none(),
                "unexpected hint for {error:?}"
            );
        }
    }
}
