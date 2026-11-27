//! Invite management: claim, list and revoke.
//!
//! Moved out of `engine.rs` unchanged (CR-P2-08, batch U review).

use super::*;

// ---- invite management --------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_claim(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    lookup_digest: Digest32,
    ttl_secs: u32,
    owner_endpoint: Option<EndpointBlob>,
    max_redeems: u32,
) {
    if ttl_secs != 0
        && !(crate::server_v2::config::INVITE_TTL_MIN_SECS
            ..=crate::server_v2::config::INVITE_TTL_MAX_SECS)
            .contains(&ttl_secs)
    {
        deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                PROTOCOL_UNSUPPORTED,
                0,
            ),
        );
        return;
    }
    let now = now_ms();
    match shared.store.claim(
        &me,
        &request_id,
        &lookup_digest,
        ttl_secs,
        max_redeems,
        owner_endpoint.as_ref().map(|blob| blob.0.as_slice()),
        now,
    ) {
        Ok(ClaimOutcome::Claimed {
            code_id,
            created_ms,
            expires_ms,
            revision,
        }) => {
            deliver_msg(
                out,
                CtrlMessage::InviteClaimed {
                    request_id,
                    code_id,
                    created_ms,
                    expires_ms,
                    revision,
                },
            );
        }
        Ok(ClaimOutcome::Replay(bytes)) => deliver_msg(out, decode_stored(&bytes)),
        Ok(ClaimOutcome::RequestConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::CodeConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                CODE_CONFLICT,
                0,
            ),
        ),
        Ok(ClaimOutcome::RateLimited) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RATE_LIMITED,
                60_000,
            ),
        ),
        Ok(ClaimOutcome::ResourceLimit) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                RESOURCE_LIMIT,
                0,
            ),
        ),
        Err(e) => {
            log::error!("claim storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

/// Decode a frame the server itself produced and journaled. Invariant:
/// these bytes round-tripped `try_encode_as`; failure is a bug, not an
/// input, so it surfaces as a storage error reply.
fn decode_stored(bytes: &[u8]) -> CtrlMessage {
    CtrlFrame::decode(bytes)
        .map(|frame| frame.message)
        .unwrap_or(CtrlMessage::ServerError {
            code: STORAGE_UNAVAILABLE,
            message: "stored response unreadable".to_string(),
        })
}

pub(super) fn handle_list(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    cursor: CodeId,
    limit: u8,
    state_filter: StateFilter,
) {
    let now = now_ms();
    match shared.store.list(&me, &cursor, limit, state_filter, now) {
        Ok(entries) => {
            let next_cursor = if entries.len() == limit as usize {
                entries.last().map(|e| e.code_id).unwrap_or(CURSOR_END)
            } else {
                CURSOR_END
            };
            deliver_msg(
                out,
                CtrlMessage::InviteListPage {
                    request_id,
                    next_cursor,
                    entries,
                },
            );
        }
        Err(e) => {
            log::error!("list storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}

pub(super) fn handle_revoke(
    shared: &Shared,
    out: &mpsc::Sender<CtrlFrame>,
    me: DeviceId,
    request_id: RequestId,
    code_id: CodeId,
    expected_revision: u64,
) {
    let now = now_ms();
    match shared
        .store
        .revoke(&me, &request_id, &code_id, expected_revision, now)
    {
        Ok(RevokeOutcome::Revoked {
            revision,
            revoked_ms,
            aborted,
        }) => {
            deliver_msg(
                out,
                CtrlMessage::InviteRevoked {
                    request_id,
                    code_id,
                    revision,
                    revoked_ms,
                },
            );
            // Q-N3 (review 20): each notification carries its own numbered
            // line, as the sweep's and the explicit abort's do.
            for pairing in aborted {
                shared.deliver_noted(
                    &pairing.owner,
                    "owner",
                    pairing.pairing_id,
                    "revoke of the code: the pairing was aborted",
                    unavailable([0u8; 16], pairing.pairing_id),
                );
                shared.deliver_noted(
                    &pairing.redeemer,
                    "redeemer",
                    pairing.pairing_id,
                    "revoke of the code: the pairing was aborted",
                    unavailable([0u8; 16], pairing.pairing_id),
                );
            }
        }
        Ok(RevokeOutcome::Replay(bytes)) => deliver_msg(out, decode_stored(&bytes)),
        Ok(RevokeOutcome::RequestConflict) => deliver_msg(
            out,
            pair_error(
                request_id,
                ErrorScope::Request,
                request_id,
                REQUEST_CONFLICT,
                0,
            ),
        ),
        Ok(RevokeOutcome::RevisionConflict) => deliver_msg(
            out,
            pair_error(request_id, ErrorScope::Code, code_id, REVISION_CONFLICT, 0),
        ),
        Ok(RevokeOutcome::Unavailable) => {
            // Unknown, foreign, or terminal — one answer.
            deliver_msg(
                out,
                pair_error(request_id, ErrorScope::Code, code_id, PAIR_UNAVAILABLE, 0),
            );
        }
        Err(e) => {
            log::error!("revoke storage failure: {e}");
            deliver_msg(
                out,
                pair_error(
                    request_id,
                    ErrorScope::Request,
                    request_id,
                    STORAGE_UNAVAILABLE,
                    0,
                ),
            );
        }
    }
}
