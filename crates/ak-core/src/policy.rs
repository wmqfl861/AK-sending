//! Fail-closed route selection model, not a real network path verifier.

/// Directness and LAN classification must come from a trusted transport adapter.
/// A private IP address, SSID, or an untrusted signaling claim is insufficient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    LocalDirect,
    InternetDirect,
    Relay,
    Unknown,
}

/// Model input only. No caller-supplied flag proves a secure connection.
#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub path: PathKind,
    pub peer_id: [u8; 32],
    pub authenticated: bool,
    pub encrypted: bool,
    pub receive_authorized: bool,
    pub cellular: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    LocalProbePending,
    InternetDisabled,
    DirectUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The index is only meaningful for the unchanged input observations.
    /// Re-run the decision after every path, identity, or permission change.
    Select { index: usize, path: PathKind },
    Wait(WaitReason),
}

/// There is intentionally no allow_relay or allow_server_file_upload setting.
#[derive(Debug, Clone, Copy)]
pub struct DirectPolicy {
    pub allow_internet: bool,
    pub allow_cellular: bool,
}

impl Default for DirectPolicy {
    fn default() -> Self {
        Self { allow_internet: true, allow_cellular: false }
    }
}

impl DirectPolicy {
    pub fn choose(
        &self,
        expected_peer: [u8; 32],
        observations: &[Observation],
        local_probe_complete: bool,
    ) -> Decision {
        let permitted = |o: &Observation| {
            o.peer_id == expected_peer
                && o.authenticated
                && o.encrypted
                && o.receive_authorized
                && (!o.cellular || self.allow_cellular)
        };
        if let Some(index) = observations.iter().position(|o| {
            permitted(o) && o.path == PathKind::LocalDirect
        }) {
            return Decision::Select { index, path: PathKind::LocalDirect };
        }
        if !local_probe_complete {
            return Decision::Wait(WaitReason::LocalProbePending);
        }
        if !self.allow_internet {
            return Decision::Wait(WaitReason::InternetDisabled);
        }
        if let Some(index) = observations.iter().position(|o| {
            permitted(o) && o.path == PathKind::InternetDirect
        }) {
            return Decision::Select { index, path: PathKind::InternetDirect };
        }
        Decision::Wait(WaitReason::DirectUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PEER: [u8; 32] = [7; 32];
    fn observation(path: PathKind) -> Observation {
        Observation {
            path, peer_id: PEER, authenticated: true, encrypted: true,
            receive_authorized: true, cellular: false,
        }
    }

    #[test]
    fn local_precedes_internet_even_when_internet_is_listed_first() {
        let candidates = [observation(PathKind::InternetDirect), observation(PathKind::LocalDirect)];
        assert_eq!(DirectPolicy::default().choose(PEER, &candidates, true),
                   Decision::Select { index: 1, path: PathKind::LocalDirect });
    }

    #[test]
    fn relay_and_unknown_are_never_accepted() {
        for path in [PathKind::Relay, PathKind::Unknown] {
            assert_eq!(DirectPolicy::default().choose(PEER, &[observation(path)], true),
                       Decision::Wait(WaitReason::DirectUnavailable));
        }
    }

    #[test]
    fn incomplete_local_probe_prevents_premature_public_selection() {
        assert_eq!(DirectPolicy::default().choose(PEER, &[observation(PathKind::InternetDirect)], false),
                   Decision::Wait(WaitReason::LocalProbePending));
    }

    #[test]
    fn verified_local_does_not_wait_for_other_candidates() {
        assert_eq!(DirectPolicy::default().choose(PEER, &[observation(PathKind::LocalDirect)], false),
                   Decision::Select { index: 0, path: PathKind::LocalDirect });
    }

    #[test]
    fn public_direct_is_accepted_after_local_probe() {
        assert_eq!(DirectPolicy::default().choose(PEER, &[observation(PathKind::InternetDirect)], true),
                   Decision::Select { index: 0, path: PathKind::InternetDirect });
    }

    #[test]
    fn identity_encryption_authentication_and_consent_are_each_required() {
        let good = observation(PathKind::LocalDirect);
        let bad = [
            Observation { peer_id: [9; 32], ..good },
            Observation { authenticated: false, ..good },
            Observation { encrypted: false, ..good },
            Observation { receive_authorized: false, ..good },
        ];
        for candidate in bad {
            assert_eq!(DirectPolicy::default().choose(PEER, &[candidate], true),
                       Decision::Wait(WaitReason::DirectUnavailable));
        }
    }

    #[test]
    fn local_only_disallows_internet_but_not_local() {
        let policy = DirectPolicy { allow_internet: false, allow_cellular: false };
        assert_eq!(policy.choose(PEER, &[observation(PathKind::InternetDirect)], true),
                   Decision::Wait(WaitReason::InternetDisabled));
        assert_eq!(policy.choose(PEER, &[observation(PathKind::LocalDirect)], true),
                   Decision::Select { index: 0, path: PathKind::LocalDirect });
    }

    #[test]
    fn cellular_requires_explicit_permission() {
        let o = Observation { cellular: true, ..observation(PathKind::InternetDirect) };
        assert_eq!(DirectPolicy::default().choose(PEER, &[o], true),
                   Decision::Wait(WaitReason::DirectUnavailable));
        let allowed = DirectPolicy { allow_cellular: true, ..DirectPolicy::default() };
        assert_eq!(allowed.choose(PEER, &[o], true),
                   Decision::Select { index: 0, path: PathKind::InternetDirect });
    }

    #[test]
    fn changed_path_is_rejected_on_reassessment() {
        let policy = DirectPolicy::default();
        let mut o = observation(PathKind::InternetDirect);
        assert!(matches!(policy.choose(PEER, &[o], true), Decision::Select { .. }));
        o.path = PathKind::Unknown;
        assert_eq!(policy.choose(PEER, &[o], true), Decision::Wait(WaitReason::DirectUnavailable));
    }

    #[test]
    fn no_candidates_means_no_connection() {
        assert_eq!(DirectPolicy::default().choose(PEER, &[], true),
                   Decision::Wait(WaitReason::DirectUnavailable));
    }
}
