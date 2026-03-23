use super::syrup::{self, DecodeError, SyrupValue};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SwissNum(pub [u8; 16]);

impl SwissNum {
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        for b in &mut bytes {
            *b = rand::random();
        }
        SwissNum(bytes)
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for i in 0..16 {
            bytes[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(SwissNum(bytes))
    }

    pub fn to_syrup(&self) -> SyrupValue {
        SyrupValue::Bytestring(self.0.to_vec())
    }

    pub fn from_syrup(val: &SyrupValue) -> Option<Self> {
        match val {
            SyrupValue::Bytestring(b) if b.len() == 16 => {
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(b);
                Some(SwissNum(bytes))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Descriptor {
    Export(u64),
    ImportObject(u64),
}

impl Descriptor {
    pub fn to_syrup(&self) -> SyrupValue {
        match self {
            Descriptor::Export(pos) => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("desc:export".into())),
                fields: vec![SyrupValue::Integer(*pos as i64)],
            },
            Descriptor::ImportObject(pos) => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("desc:import-object".into())),
                fields: vec![SyrupValue::Integer(*pos as i64)],
            },
        }
    }

    pub fn from_syrup(val: &SyrupValue) -> Option<Self> {
        match val {
            SyrupValue::Record { label, fields } => {
                if let SyrupValue::Symbol(s) = label.as_ref() {
                    match s.as_str() {
                        "desc:export" => {
                            if let Some(SyrupValue::Integer(n)) = fields.first() {
                                return Some(Descriptor::Export(*n as u64));
                            }
                        }
                        "desc:import-object" => {
                            if let Some(SyrupValue::Integer(n)) = fields.first() {
                                return Some(Descriptor::ImportObject(*n as u64));
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OCapNMessage {
    OpStartSession {
        session_pubkey: Vec<u8>,
    },
    OpDeliverOnly {
        to_desc: Descriptor,
        args: Vec<SyrupValue>,
    },
    OpDeliver {
        to_desc: Descriptor,
        args: Vec<SyrupValue>,
        request_id: u64,
    },
    OpDeliverResult {
        request_id: u64,
        value: SyrupValue,
    },
    OpAbort {
        reason: String,
    },
}

impl OCapNMessage {
    pub fn encode(&self) -> Vec<u8> {
        syrup::encode(&self.to_syrup())
    }

    pub fn decode(data: &[u8]) -> Result<Self, DecodeError> {
        let (val, _) = syrup::decode(data)?;
        Self::from_syrup(&val).ok_or(DecodeError::InvalidByte(0))
    }

    pub fn to_syrup(&self) -> SyrupValue {
        match self {
            OCapNMessage::OpStartSession { session_pubkey } => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("op:start-session".into())),
                fields: vec![SyrupValue::Bytestring(session_pubkey.clone())],
            },
            OCapNMessage::OpDeliverOnly { to_desc, args } => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("op:deliver-only".into())),
                fields: vec![
                    to_desc.to_syrup(),
                    SyrupValue::List(args.clone()),
                ],
            },
            OCapNMessage::OpDeliver { to_desc, args, request_id } => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("op:deliver".into())),
                fields: vec![
                    to_desc.to_syrup(),
                    SyrupValue::List(args.clone()),
                    SyrupValue::Integer(*request_id as i64),
                ],
            },
            OCapNMessage::OpDeliverResult { request_id, value } => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("op:deliver-result".into())),
                fields: vec![
                    SyrupValue::Integer(*request_id as i64),
                    value.clone(),
                ],
            },
            OCapNMessage::OpAbort { reason } => SyrupValue::Record {
                label: Box::new(SyrupValue::Symbol("op:abort".into())),
                fields: vec![SyrupValue::String(reason.clone())],
            },
        }
    }

    pub fn from_syrup(val: &SyrupValue) -> Option<Self> {
        match val {
            SyrupValue::Record { label, fields } => {
                if let SyrupValue::Symbol(s) = label.as_ref() {
                    match s.as_str() {
                        "op:start-session" => {
                            if let Some(SyrupValue::Bytestring(pk)) = fields.first() {
                                return Some(OCapNMessage::OpStartSession {
                                    session_pubkey: pk.clone(),
                                });
                            }
                        }
                        "op:deliver-only" => {
                            if fields.len() >= 2 {
                                let desc = Descriptor::from_syrup(&fields[0])?;
                                let args = match &fields[1] {
                                    SyrupValue::List(a) => a.clone(),
                                    _ => return None,
                                };
                                return Some(OCapNMessage::OpDeliverOnly {
                                    to_desc: desc,
                                    args,
                                });
                            }
                        }
                        "op:deliver" => {
                            if fields.len() >= 3 {
                                let desc = Descriptor::from_syrup(&fields[0])?;
                                let args = match &fields[1] {
                                    SyrupValue::List(a) => a.clone(),
                                    _ => return None,
                                };
                                let request_id = match &fields[2] {
                                    SyrupValue::Integer(n) => *n as u64,
                                    _ => return None,
                                };
                                return Some(OCapNMessage::OpDeliver {
                                    to_desc: desc,
                                    args,
                                    request_id,
                                });
                            }
                        }
                        "op:deliver-result" => {
                            if fields.len() >= 2 {
                                let request_id = match &fields[0] {
                                    SyrupValue::Integer(n) => *n as u64,
                                    _ => return None,
                                };
                                return Some(OCapNMessage::OpDeliverResult {
                                    request_id,
                                    value: fields[1].clone(),
                                });
                            }
                        }
                        "op:abort" => {
                            if let Some(SyrupValue::String(r)) = fields.first() {
                                return Some(OCapNMessage::OpAbort {
                                    reason: r.clone(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swiss_num_roundtrip() {
        let sn = SwissNum::random();
        let hex = sn.to_hex();
        let back = SwissNum::from_hex(&hex).unwrap();
        assert_eq!(sn, back);
    }

    #[test]
    fn test_op_deliver_roundtrip() {
        let msg = OCapNMessage::OpDeliver {
            to_desc: Descriptor::Export(0),
            args: vec![
                SyrupValue::Symbol("deliver-to".into()),
                SyrupValue::Bytestring(vec![0xAB; 16]),
                SyrupValue::Symbol("get".into()),
            ],
            request_id: 12345,
        };
        let bytes = msg.encode();
        let decoded = OCapNMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }
}
