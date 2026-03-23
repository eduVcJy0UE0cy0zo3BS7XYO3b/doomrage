use super::syrup::SyrupValue;
use super::types::{Descriptor, SwissNum};
use std::collections::HashMap;

pub trait OCapNObject: Send + Sync {
    fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String>;
}

struct ExportEntry {
    swiss: SwissNum,
    object: Box<dyn OCapNObject>,
}

pub struct Session {
    exports: Vec<ExportEntry>,
    export_by_swiss: HashMap<SwissNum, usize>,
    pub export_by_key: HashMap<String, SwissNum>,
    imports: Vec<Option<SyrupValue>>,
}

impl Session {
    pub fn new(bootstrap: Box<dyn OCapNObject>) -> Self {
        let swiss = SwissNum::random();
        let mut export_by_swiss = HashMap::new();
        export_by_swiss.insert(swiss.clone(), 0);

        let mut session = Session {
            exports: Vec::new(),
            export_by_swiss,
            export_by_key: HashMap::new(),
            imports: Vec::new(),
        };
        session.exports.push(ExportEntry {
            swiss,
            object: bootstrap,
        });
        session
    }

    pub fn export_object(&mut self, object: Box<dyn OCapNObject>) -> (u64, SwissNum) {
        let pos = self.exports.len() as u64;
        let swiss = SwissNum::random();
        self.export_by_swiss.insert(swiss.clone(), pos as usize);
        self.exports.push(ExportEntry {
            swiss: swiss.clone(),
            object,
        });
        (pos, swiss)
    }

    pub fn export_object_keyed(&mut self, key: String, object: Box<dyn OCapNObject>) -> (u64, SwissNum) {
        if let Some(swiss) = self.export_by_key.get(&key) {
            let pos = self.export_by_swiss[swiss];
            self.exports[pos].object = object;
            return (pos as u64, swiss.clone());
        }
        let (pos, swiss) = self.export_object(object);
        self.export_by_key.insert(key, swiss.clone());
        (pos, swiss)
    }

    pub fn deliver(&self, desc: &Descriptor, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
        match desc {
            Descriptor::Export(pos) => {
                let entry = self.exports.get(*pos as usize)
                    .ok_or_else(|| format!("no export at position {}", pos))?;
                entry.object.deliver(args)
            }
            Descriptor::ImportObject(pos) => {
                Err(format!("cannot deliver to import {}: not a local object", pos))
            }
        }
    }

    pub fn lookup_by_swiss(&self, swiss: &SwissNum) -> Option<u64> {
        self.export_by_swiss.get(swiss).map(|&pos| pos as u64)
    }

    pub fn add_import(&mut self, value: SyrupValue) -> u64 {
        let pos = self.imports.len() as u64;
        self.imports.push(Some(value));
        pos
    }

    pub fn bootstrap_swiss(&self) -> &SwissNum {
        &self.exports[0].swiss
    }
}

pub struct BootstrapObject {
    swiss_to_pos: HashMap<SwissNum, u64>,
}

impl BootstrapObject {
    pub fn new() -> Self {
        BootstrapObject {
            swiss_to_pos: HashMap::new(),
        }
    }

    pub fn register(&mut self, swiss: SwissNum, pos: u64) {
        self.swiss_to_pos.insert(swiss, pos);
    }
}

impl OCapNObject for BootstrapObject {
    fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
        if args.len() < 2 {
            return Err("bootstrap: expected (fetch swiss-num)".into());
        }
        match (&args[0], &args[1]) {
            (SyrupValue::Symbol(method), swiss_val) if method == "fetch" => {
                let swiss = SwissNum::from_syrup(swiss_val)
                    .ok_or("bootstrap: invalid swiss-num")?;
                match self.swiss_to_pos.get(&swiss) {
                    Some(pos) => Ok(Some(Descriptor::Export(*pos).to_syrup())),
                    None => Err("bootstrap: swiss-num not found".into()),
                }
            }
            _ => Err(format!("bootstrap: unknown method {:?}", args[0])),
        }
    }
}

pub struct SessionManager {
    sessions: HashMap<String, Session>,
    local_session: Session,
    local_peer_id: Option<String>,
}

impl SessionManager {
    pub fn new() -> Self {
        SessionManager {
            sessions: HashMap::new(),
            local_session: Session::new(Box::new(LocalBootstrap)),
            local_peer_id: None,
        }
    }

    pub fn set_local_peer_id(&mut self, peer_id: String) {
        self.local_peer_id = Some(peer_id);
    }

    pub fn local_peer_id(&self) -> Option<&str> {
        self.local_peer_id.as_deref()
    }

    pub fn local_session(&self) -> &Session {
        &self.local_session
    }

    pub fn local_session_mut(&mut self) -> &mut Session {
        &mut self.local_session
    }

    pub fn deliver_by_swiss(
        &self,
        swiss: &SwissNum,
        args: &[SyrupValue],
    ) -> Result<Option<SyrupValue>, String> {
        let pos = self.local_session.lookup_by_swiss(swiss)
            .ok_or_else(|| format!("swiss-num {} not found in local exports", swiss.to_hex()))?;
        self.local_session.deliver(&Descriptor::Export(pos), args)
    }

    pub fn create_session(&mut self, peer_id: &str, bootstrap: Box<dyn OCapNObject>) -> &mut Session {
        self.sessions.entry(peer_id.to_string())
            .or_insert_with(|| Session::new(bootstrap))
    }

    pub fn get_session(&self, peer_id: &str) -> Option<&Session> {
        self.sessions.get(peer_id)
    }

    pub fn get_session_mut(&mut self, peer_id: &str) -> Option<&mut Session> {
        self.sessions.get_mut(peer_id)
    }

    pub fn remove_session(&mut self, peer_id: &str) {
        self.sessions.remove(peer_id);
    }

    pub fn ensure_session(&mut self, peer_id: &str) -> &mut Session {
        self.sessions.entry(peer_id.to_string())
            .or_insert_with(|| Session::new(Box::new(LocalBootstrap)))
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

struct LocalBootstrap;

impl OCapNObject for LocalBootstrap {
    fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
        Ok(Some(SyrupValue::List(args.to_vec())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoObject;

    impl OCapNObject for EchoObject {
        fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
            Ok(Some(SyrupValue::List(args.to_vec())))
        }
    }

    struct ValueHolder(SyrupValue);

    impl OCapNObject for ValueHolder {
        fn deliver(&self, _args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
            Ok(Some(self.0.clone()))
        }
    }

    #[test]
    fn test_session_export_and_deliver() {
        let mut session = Session::new(Box::new(EchoObject));
        let result = session.deliver(
            &Descriptor::Export(0),
            &[SyrupValue::String("hello".into())],
        ).unwrap();
        assert_eq!(result, Some(SyrupValue::List(vec![SyrupValue::String("hello".into())])));

        let (pos, swiss) = session.export_object(Box::new(ValueHolder(SyrupValue::Integer(42))));
        assert_eq!(pos, 1);

        let result = session.deliver(&Descriptor::Export(pos), &[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(42)));
        assert_eq!(session.lookup_by_swiss(&swiss), Some(1));
    }

    #[test]
    fn test_session_manager_lifecycle() {
        let mut mgr = SessionManager::new();
        mgr.set_local_peer_id("peer-abc".into());
        assert_eq!(mgr.session_count(), 0);

        mgr.create_session("remote-1", Box::new(EchoObject));
        assert_eq!(mgr.session_count(), 1);

        mgr.remove_session("remote-1");
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn test_deliver_by_swiss() {
        let mut mgr = SessionManager::new();
        let (_, swiss) = mgr.local_session_mut()
            .export_object(Box::new(ValueHolder(SyrupValue::Integer(42))));
        let result = mgr.deliver_by_swiss(&swiss, &[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(42)));
    }
}
