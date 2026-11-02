use crate::{Error, Result};

pub(super) const HEADER_SIZE: usize = 68;
pub const MAX_BATCH_BYTES: usize = 16 * 1024;
const MAGIC: &[u8; 4] = b"AKS1";
const VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Kind {
    Hello = 1,
    Ready = 2,
    Data = 3,
    Ack = 4,
    End = 5,
    EndAck = 6,
}

impl TryFrom<u8> for Kind {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Ready),
            3 => Ok(Self::Data),
            4 => Ok(Self::Ack),
            5 => Ok(Self::End),
            6 => Ok(Self::EndAck),
            _ => Err(Error::Protocol),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Context {
    pub task: [u8; 16],
    pub binding: [u8; 32],
}

pub(super) struct Header {
    pub kind: Kind,
    pub sequence: u64,
    pub length: usize,
}

impl Header {
    pub fn encode(&self, context: Context) -> Result<[u8; HEADER_SIZE]> {
        self.validate()?;
        let mut bytes = [0; HEADER_SIZE];
        bytes[..4].copy_from_slice(MAGIC);
        bytes[4] = VERSION;
        bytes[5] = self.kind as u8;
        bytes[8..16].copy_from_slice(&self.sequence.to_be_bytes());
        bytes[16..32].copy_from_slice(&context.task);
        bytes[32..64].copy_from_slice(&context.binding);
        bytes[64..68].copy_from_slice(&(self.length as u32).to_be_bytes());
        Ok(bytes)
    }

    /// Validate the complete fixed header before allocating or reading a body.
    pub fn decode(bytes: &[u8; HEADER_SIZE], context: Context) -> Result<Self> {
        if &bytes[..4] != MAGIC
            || bytes[4] != VERSION
            || bytes[6..8] != [0, 0]
            || bytes[16..32] != context.task
            || bytes[32..64] != context.binding
        {
            return Err(Error::Protocol);
        }
        let header = Self {
            kind: Kind::try_from(bytes[5])?,
            sequence: u64::from_be_bytes(bytes[8..16].try_into().map_err(|_| Error::Protocol)?),
            length: u32::from_be_bytes(bytes[64..68].try_into().map_err(|_| Error::Protocol)?)
                as usize,
        };
        header.validate()?;
        Ok(header)
    }

    fn validate(&self) -> Result<()> {
        let sequence_ok = match self.kind {
            Kind::Hello | Kind::Ready => self.sequence == 0,
            _ => self.sequence != 0,
        };
        let length_ok = if self.kind == Kind::Data {
            (1..=MAX_BATCH_BYTES).contains(&self.length)
        } else {
            self.length == 0
        };
        if sequence_ok && length_ok {
            Ok(())
        } else {
            Err(Error::Protocol)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: Context = Context {
        task: [7; 16],
        binding: [9; 32],
    };

    #[test]
    fn every_kind_roundtrips_and_has_fixed_length() {
        for kind in [
            Kind::Hello,
            Kind::Ready,
            Kind::Data,
            Kind::Ack,
            Kind::End,
            Kind::EndAck,
        ] {
            let header = Header {
                kind,
                sequence: if matches!(kind, Kind::Hello | Kind::Ready) {
                    0
                } else {
                    42
                },
                length: if kind == Kind::Data {
                    MAX_BATCH_BYTES
                } else {
                    0
                },
            };
            let bytes = header.encode(CONTEXT).unwrap();
            assert_eq!(bytes.len(), HEADER_SIZE);
            let decoded = Header::decode(&bytes, CONTEXT).unwrap();
            assert_eq!(decoded.kind, kind);
            assert_eq!(decoded.sequence, header.sequence);
            assert_eq!(decoded.length, header.length);
        }
    }

    #[test]
    fn magic_version_flags_task_and_binding_mutations_are_rejected() {
        let bytes = Header {
            kind: Kind::Data,
            sequence: 1,
            length: 1,
        }
        .encode(CONTEXT)
        .unwrap();
        for position in (0..5).chain(6..8).chain(16..64) {
            let mut malformed = bytes;
            malformed[position] ^= 1;
            assert!(
                Header::decode(&malformed, CONTEXT).is_err(),
                "position {position}"
            );
        }
        for kind in [0, 7, 255] {
            let mut malformed = bytes;
            malformed[5] = kind;
            assert!(Header::decode(&malformed, CONTEXT).is_err());
        }
    }

    #[test]
    fn length_is_validated_before_any_allocation() {
        let bytes = Header {
            kind: Kind::Data,
            sequence: 1,
            length: 1,
        }
        .encode(CONTEXT)
        .unwrap();
        for length in [0, MAX_BATCH_BYTES as u32 + 1, u32::MAX] {
            let mut malformed = bytes;
            malformed[64..68].copy_from_slice(&length.to_be_bytes());
            assert!(Header::decode(&malformed, CONTEXT).is_err());
        }
        for kind in [Kind::Hello, Kind::Ready, Kind::Ack, Kind::End, Kind::EndAck] {
            let header = Header {
                kind,
                sequence: if matches!(kind, Kind::Hello | Kind::Ready) {
                    0
                } else {
                    1
                },
                length: 1,
            };
            assert!(header.encode(CONTEXT).is_err());
        }
    }

    #[test]
    fn sequence_zero_is_only_valid_for_handshake() {
        for kind in [Kind::Data, Kind::Ack, Kind::End, Kind::EndAck] {
            let header = Header {
                kind,
                sequence: 0,
                length: if kind == Kind::Data { 1 } else { 0 },
            };
            assert!(header.encode(CONTEXT).is_err());
        }
        for kind in [Kind::Hello, Kind::Ready] {
            assert!(Header {
                kind,
                sequence: 1,
                length: 0
            }
            .encode(CONTEXT)
            .is_err());
        }
    }

    #[test]
    fn max_sequence_has_no_wire_truncation() {
        let bytes = Header {
            kind: Kind::Ack,
            sequence: u64::MAX,
            length: 0,
        }
        .encode(CONTEXT)
        .unwrap();
        assert_eq!(Header::decode(&bytes, CONTEXT).unwrap().sequence, u64::MAX);
    }
}
