use super::syrup::SyrupValue;
use super::types::{Descriptor, SwissNum};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

    /// Export with a stable key. If the key already exists, returns the existing SwissNum
    /// and replaces the object. Otherwise creates a new export.
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

/// Bootstrap object: handles `fetch` to look up exports by SwissNum.
pub struct BootstrapObject {
    // Holds a reference back to the session's export table via a closure-like mechanism.
    // For Phase 1, we use a simple Swiss->position map snapshot approach.
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
        // Expected: (symbol"fetch", bytestring<swiss>)
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

    /// Deliver a message to a local export identified by swiss num.
    /// Used when receiving OpDeliverOnly from remote peers.
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

    /// Ensure a session exists for the given peer, creating one if needed.
    pub fn ensure_session(&mut self, peer_id: &str) -> &mut Session {
        self.sessions.entry(peer_id.to_string())
            .or_insert_with(|| Session::new(Box::new(LocalBootstrap)))
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

/// Local bootstrap: handles "deliver-to" by looking up swiss in the same session.
/// Since it can't borrow the session, actual dispatch is done by SessionManager::deliver_by_swiss.
struct LocalBootstrap;

impl OCapNObject for LocalBootstrap {
    fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
        // This is a marker; actual dispatch happens in SessionManager::deliver_by_swiss
        // via the "deliver-to" convention. If we get here directly, just echo back.
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

        // Bootstrap is at position 0
        let result = session.deliver(
            &Descriptor::Export(0),
            &[SyrupValue::String("hello".into())],
        ).unwrap();
        assert_eq!(result, Some(SyrupValue::List(vec![SyrupValue::String("hello".into())])));

        // Export a new object
        let (pos, swiss) = session.export_object(Box::new(ValueHolder(SyrupValue::Integer(42))));
        assert_eq!(pos, 1);

        // Deliver to it
        let result = session.deliver(&Descriptor::Export(pos), &[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(42)));

        // Lookup by swiss
        assert_eq!(session.lookup_by_swiss(&swiss), Some(1));
    }

    #[test]
    fn test_bootstrap_fetch() {
        let mut bootstrap = BootstrapObject::new();
        let sn = SwissNum::random();
        bootstrap.register(sn.clone(), 5);

        let result = bootstrap.deliver(&[
            SyrupValue::Symbol("fetch".into()),
            sn.to_syrup(),
        ]).unwrap();

        assert_eq!(result, Some(Descriptor::Export(5).to_syrup()));
    }

    #[test]
    fn test_bootstrap_fetch_not_found() {
        let bootstrap = BootstrapObject::new();
        let sn = SwissNum::random();
        let result = bootstrap.deliver(&[
            SyrupValue::Symbol("fetch".into()),
            sn.to_syrup(),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_session_manager_lifecycle() {
        let mut mgr = SessionManager::new();
        mgr.set_local_peer_id("peer-abc".into());

        assert_eq!(mgr.session_count(), 0);

        mgr.create_session("remote-1", Box::new(EchoObject));
        assert_eq!(mgr.session_count(), 1);
        assert!(mgr.get_session("remote-1").is_some());
        assert!(mgr.get_session("remote-2").is_none());

        mgr.remove_session("remote-1");
        assert_eq!(mgr.session_count(), 0);
    }

    #[test]
    fn test_local_export_and_deliver_by_swiss() {
        let mut mgr = SessionManager::new();
        mgr.set_local_peer_id("12D3KooW_test".into());

        // Export a value
        let (pos, swiss) = mgr.local_session_mut()
            .export_object(Box::new(ValueHolder(SyrupValue::Integer(42))));
        assert_eq!(pos, 1); // pos 0 is bootstrap

        // Deliver by swiss
        let result = mgr.deliver_by_swiss(&swiss, &[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(42)));
    }

    #[test]
    fn test_local_deliver_unknown_swiss() {
        let mgr = SessionManager::new();
        let unknown = SwissNum::random();
        let result = mgr.deliver_by_swiss(&unknown, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_end_to_end_export_send_deliver() {
        use super::super::locator::OCapNLocator;
        use super::super::types::{Descriptor, OCapNMessage};

        // --- Node A: export a value ---
        let mut mgr_a = SessionManager::new();
        mgr_a.set_local_peer_id("peer-A".into());

        let val = SyrupValue::String("secret-data".into());
        let (_pos, swiss) = mgr_a.local_session_mut()
            .export_object(Box::new(ValueHolder(val)));

        // Build URI (what ocapn-export returns)
        let uri = format!("ocapn://peer-A.libp2p/s/{}", swiss.to_hex());

        // --- Node B: parse URI and build message ---
        let locator = OCapNLocator::parse(&uri).unwrap();
        assert_eq!(locator.designator, "peer-A");
        let target_swiss = locator.swiss_num.unwrap();

        let msg = OCapNMessage::OpDeliverOnly {
            to_desc: Descriptor::Export(0), // bootstrap
            args: vec![
                SyrupValue::Symbol("deliver-to".into()),
                target_swiss.to_syrup(),
                SyrupValue::Symbol("get".into()),
            ],
        };

        // --- Simulate network: encode, decode ---
        let wire = msg.encode();
        let received = OCapNMessage::decode(&wire).unwrap();

        // --- Node A: receive and dispatch ---
        if let OCapNMessage::OpDeliverOnly { args, .. } = &received {
            assert!(args.len() >= 2);
            if let SyrupValue::Symbol(method) = &args[0] {
                assert_eq!(method, "deliver-to");
                let recv_swiss = SwissNum::from_syrup(&args[1]).unwrap();
                let remaining = &args[2..];
                let result = mgr_a.deliver_by_swiss(&recv_swiss, remaining).unwrap();
                assert_eq!(result, Some(SyrupValue::String("secret-data".into())));
            }
        } else {
            panic!("expected OpDeliverOnly");
        }
    }

    #[test]
    fn test_deliver_to_invalid_export() {
        let session = Session::new(Box::new(EchoObject));
        let result = session.deliver(&Descriptor::Export(999), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_deliver_to_import_fails() {
        let session = Session::new(Box::new(EchoObject));
        let result = session.deliver(&Descriptor::ImportObject(0), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_exported_slot_get_set() {
        use std::sync::{Arc, Mutex};

        struct SlotObject {
            value: Arc<Mutex<SyrupValue>>,
        }
        impl OCapNObject for SlotObject {
            fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
                let method = args.first().and_then(|a| {
                    if let SyrupValue::Symbol(s) = a { Some(s.as_str()) } else { None }
                });
                match method {
                    Some("set") if args.len() >= 2 => {
                        *self.value.lock().unwrap() = args[1].clone();
                        Ok(None)
                    }
                    _ => Ok(Some(self.value.lock().unwrap().clone())),
                }
            }
        }

        let val = Arc::new(Mutex::new(SyrupValue::Integer(42)));
        let slot = SlotObject { value: Arc::clone(&val) };

        // Get initial value
        let result = slot.deliver(&[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(42)));

        // Set new value
        let result = slot.deliver(&[
            SyrupValue::Symbol("set".into()),
            SyrupValue::Integer(100),
        ]).unwrap();
        assert_eq!(result, None);

        // Get updated value
        let result = slot.deliver(&[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(100)));

        // Shared Arc reflects the change
        assert_eq!(*val.lock().unwrap(), SyrupValue::Integer(100));
    }

    #[test]
    fn test_exported_slot_via_session_manager() {
        use std::sync::{Arc, Mutex};

        struct SlotObject {
            value: Arc<Mutex<SyrupValue>>,
        }
        impl OCapNObject for SlotObject {
            fn deliver(&self, args: &[SyrupValue]) -> Result<Option<SyrupValue>, String> {
                let method = args.first().and_then(|a| {
                    if let SyrupValue::Symbol(s) = a { Some(s.as_str()) } else { None }
                });
                match method {
                    Some("set") if args.len() >= 2 => {
                        *self.value.lock().unwrap() = args[1].clone();
                        Ok(None)
                    }
                    _ => Ok(Some(self.value.lock().unwrap().clone())),
                }
            }
        }

        let mut mgr = SessionManager::new();
        mgr.set_local_peer_id("test-peer".into());

        let val = Arc::new(Mutex::new(SyrupValue::String("hello".into())));
        let slot = SlotObject { value: Arc::clone(&val) };
        let (_pos, swiss) = mgr.local_session_mut().export_object(Box::new(slot));

        // Remote "set" via deliver_by_swiss
        let result = mgr.deliver_by_swiss(&swiss, &[
            SyrupValue::Symbol("set".into()),
            SyrupValue::String("world".into()),
        ]).unwrap();
        assert_eq!(result, None);

        // Read back via shared Arc (simulating ocapn-receive reading the slot store)
        assert_eq!(*val.lock().unwrap(), SyrupValue::String("world".into()));

        // Get via deliver
        let result = mgr.deliver_by_swiss(&swiss, &[]).unwrap();
        assert_eq!(result, Some(SyrupValue::String("world".into())));
    }

    #[test]
    fn test_export_object_keyed_stable() {
        let mut session = Session::new(Box::new(EchoObject));

        // First keyed export
        let (pos1, swiss1) = session.export_object_keyed(
            "node:1:export:0".into(),
            Box::new(ValueHolder(SyrupValue::Integer(42))),
        );
        assert_eq!(pos1, 1);

        // Same key → same swiss, updated object
        let (pos2, swiss2) = session.export_object_keyed(
            "node:1:export:0".into(),
            Box::new(ValueHolder(SyrupValue::Integer(100))),
        );
        assert_eq!(pos2, pos1);
        assert_eq!(swiss2, swiss1);

        // Deliver returns updated value
        let result = session.deliver(&Descriptor::Export(pos2), &[]).unwrap();
        assert_eq!(result, Some(SyrupValue::Integer(100)));

        // Different key → different export
        let (pos3, swiss3) = session.export_object_keyed(
            "node:1:export:1".into(),
            Box::new(ValueHolder(SyrupValue::Integer(200))),
        );
        assert_ne!(pos3, pos1);
        assert_ne!(swiss3, swiss1);
    }

    #[test]
    fn test_ensure_session_idempotent() {
        let mut mgr = SessionManager::new();
        assert_eq!(mgr.session_count(), 0);

        mgr.ensure_session("peer-1");
        assert_eq!(mgr.session_count(), 1);

        // Second call doesn't create duplicate
        mgr.ensure_session("peer-1");
        assert_eq!(mgr.session_count(), 1);

        mgr.ensure_session("peer-2");
        assert_eq!(mgr.session_count(), 2);
    }

    #[test]
    fn test_session_cleanup_on_remove() {
        let mut mgr = SessionManager::new();
        mgr.ensure_session("peer-1");
        mgr.ensure_session("peer-2");
        assert_eq!(mgr.session_count(), 2);

        mgr.remove_session("peer-1");
        assert_eq!(mgr.session_count(), 1);
        assert!(mgr.get_session("peer-1").is_none());
        assert!(mgr.get_session("peer-2").is_some());
    }
}
