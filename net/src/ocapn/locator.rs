use super::types::SwissNum;

#[derive(Debug, Clone, PartialEq)]
pub struct OCapNLocator {
    pub designator: String,
    pub transport: String,
    pub swiss_num: Option<SwissNum>,
}

impl OCapNLocator {
    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("ocapn://")?;

        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };

        let dot_pos = authority.rfind('.')?;
        let designator = authority[..dot_pos].to_string();
        let transport = authority[dot_pos + 1..].to_string();

        if designator.is_empty() || transport.is_empty() {
            return None;
        }

        let swiss_num = if let Some(hex) = path.strip_prefix("/s/") {
            SwissNum::from_hex(hex)
        } else {
            None
        };

        Some(OCapNLocator {
            designator,
            transport,
            swiss_num,
        })
    }

    pub fn to_uri(&self) -> String {
        let base = format!("ocapn://{}.{}", self.designator, self.transport);
        match &self.swiss_num {
            Some(sn) => format!("{}/s/{}", base, sn.to_hex()),
            None => base,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_with_swiss() {
        let sn = SwissNum([0xab; 16]);
        let uri = format!("ocapn://12D3peer.libp2p/s/{}", sn.to_hex());
        let loc = OCapNLocator::parse(&uri).unwrap();
        assert_eq!(loc.designator, "12D3peer");
        assert_eq!(loc.transport, "libp2p");
        assert_eq!(loc.swiss_num, Some(sn));
    }

    #[test]
    fn test_roundtrip() {
        let sn = SwissNum::random();
        let loc = OCapNLocator {
            designator: "peer123".into(),
            transport: "libp2p".into(),
            swiss_num: Some(sn),
        };
        let uri = loc.to_uri();
        let parsed = OCapNLocator::parse(&uri).unwrap();
        assert_eq!(loc, parsed);
    }
}
