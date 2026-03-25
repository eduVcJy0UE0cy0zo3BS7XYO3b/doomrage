use crate::bencode::{self, Value};
use std::io::BufReader;
use std::net::TcpStream;

/// Simple nREPL client for tests and CLI usage.
pub struct Client {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    msg_counter: u64,
}

impl Client {
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Client {
            reader,
            writer: stream,
            msg_counter: 0,
        })
    }

    fn next_id(&mut self) -> String {
        self.msg_counter += 1;
        format!("msg-{}", self.msg_counter)
    }

    pub fn send(&mut self, msg: &Value) -> std::io::Result<()> {
        use std::io::Write;
        let data = bencode::encode_to_vec(msg);
        self.writer.write_all(&data)?;
        self.writer.flush()
    }

    pub fn recv(&mut self) -> Result<Value, bencode::DecodeError> {
        bencode::decode(&mut self.reader)
    }

    /// Receive all responses until we see a "done" status.
    pub fn recv_until_done(&mut self) -> Result<Vec<Value>, bencode::DecodeError> {
        let mut responses = Vec::new();
        loop {
            let resp = self.recv()?;
            let is_done = resp
                .get("status")
                .and_then(|s| s.as_list())
                .map_or(false, |list| list.iter().any(|v| v.as_str() == Some("done")));
            responses.push(resp);
            if is_done { break; }
        }
        Ok(responses)
    }

    /// Clone a new session, returns session ID.
    pub fn clone_session(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("id", Value::string(&id)),
            ("op", Value::string("clone")),
        ]))?;
        let responses = self.recv_until_done()?;
        let session = responses.last()
            .and_then(|r| r.get_str("new-session"))
            .ok_or("no new-session in clone response")?
            .to_string();
        Ok(session)
    }

    /// Eval code in a session, returns all response messages.
    pub fn eval(&mut self, session: &str, code: &str) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("code", Value::string(code)),
            ("id", Value::string(&id)),
            ("op", Value::string("eval")),
            ("session", Value::string(session)),
        ]))?;
        Ok(self.recv_until_done()?)
    }

    /// Describe server capabilities.
    pub fn describe(&mut self) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("id", Value::string(&id)),
            ("op", Value::string("describe")),
        ]))?;
        let responses = self.recv_until_done()?;
        Ok(responses.into_iter().last().unwrap())
    }

    /// Get completions for a prefix.
    pub fn completions(&mut self, session: &str, prefix: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("id", Value::string(&id)),
            ("op", Value::string("completions")),
            ("prefix", Value::string(prefix)),
            ("session", Value::string(session)),
        ]))?;
        let responses = self.recv_until_done()?;
        Ok(responses.into_iter().last().unwrap())
    }

    /// Get info about a symbol.
    pub fn info(&mut self, session: &str, symbol: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("id", Value::string(&id)),
            ("op", Value::string("info")),
            ("session", Value::string(session)),
            ("symbol", Value::string(symbol)),
        ]))?;
        let responses = self.recv_until_done()?;
        Ok(responses.into_iter().last().unwrap())
    }

    /// List available namespaces.
    pub fn ns_list(&mut self, session: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("id", Value::string(&id)),
            ("op", Value::string("ns-list")),
            ("session", Value::string(session)),
        ]))?;
        let responses = self.recv_until_done()?;
        Ok(responses.into_iter().last().unwrap())
    }

    /// Switch session to a namespace.
    pub fn switch_ns(&mut self, session: &str, ns: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("id", Value::string(&id)),
            ("ns", Value::string(ns)),
            ("op", Value::string("switch-ns")),
            ("session", Value::string(session)),
        ]))?;
        let responses = self.recv_until_done()?;
        Ok(responses.into_iter().last().unwrap())
    }

    /// Load a file.
    pub fn load_file(&mut self, session: &str, path: &str, content: &str) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let id = self.next_id();
        self.send(&Value::dict(vec![
            ("file", Value::string(content)),
            ("file-path", Value::string(path)),
            ("id", Value::string(&id)),
            ("op", Value::string("load-file")),
            ("session", Value::string(session)),
        ]))?;
        Ok(self.recv_until_done()?)
    }
}
