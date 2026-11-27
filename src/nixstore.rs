//! Minimal Nix ATerm derivation builder and protocol-1.21 Unix-socket daemon client.

use crate::sha256::{Sha256, hex_lower};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io::{BufReader, BufWriter, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    sync::{Arc, Mutex},
    thread,
};

pub type Result<T> = std::result::Result<T, String>;

const NIX_BASE32_CHARS: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";
const CLIENT_MAGIC: u64 = 0x6e69_7863;
const SERVER_MAGIC: u64 = 0x6478_696f;
const PROTOCOL_1_21: u64 = (1 << 8) | 21;
const STDERR_NEXT: u64 = 0x6f6c_6d67;
const STDERR_LAST: u64 = 0x616c_7473;
const STDERR_ERROR: u64 = 0x6378_7470;
const WOP_ADD_TEXT_TO_STORE: u64 = 8;
const WOP_QUERY_VALID_PATHS: u64 = 31;

pub fn store_dir() -> String {
    match std::env::var("NIX_STORE") {
        Ok(dir) if dir.starts_with('/') => {
            let trimmed = dir.trim_end_matches('/');
            if trimmed.is_empty() {
                "/".into()
            } else {
                trimmed.into()
            }
        }
        _ => "/nix/store".into(),
    }
}

pub fn nix_base32(data: &[u8]) -> String {
    let out_len = (data.len() * 8 - 1) / 5 + 1;
    let mut out = vec![0u8; out_len];
    for (n, slot) in out.iter_mut().enumerate() {
        let b = (out_len - 1 - n) * 5;
        let i = b / 8;
        let j = b % 8;
        let mut c = data[i] >> j;
        if j > 0 && i + 1 < data.len() {
            c |= data[i + 1] << (8 - j);
        }
        *slot = NIX_BASE32_CHARS[(c & 0x1f) as usize];
    }
    String::from_utf8(out).unwrap()
}

fn compress_hash(hash: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    for (i, &b) in hash.iter().enumerate() {
        out[i % size] ^= b;
    }
    out
}

pub fn make_store_path(store_dir: &str, typ: &str, sha256_hex: &str, name: &str) -> String {
    let fingerprint = format!("{typ}:sha256:{sha256_hex}:{store_dir}:{name}");
    let sum = Sha256::digest(fingerprint.as_bytes());
    let hash = nix_base32(&compress_hash(&sum, 20));
    format!("{store_dir}/{hash}-{name}")
}

pub fn make_text_path(store_dir: &str, name: &str, text: &[u8], refs: &[String]) -> String {
    let sum = Sha256::digest(text);
    let sorted: BTreeSet<&str> = refs.iter().map(String::as_str).collect();
    let mut typ = String::from("text");
    for r in sorted {
        typ.push(':');
        typ.push_str(r);
    }
    make_store_path(store_dir, &typ, &hex_lower(&sum), name)
}

pub fn make_fixed_output_path(
    store_dir: &str,
    name: &str,
    recursive: bool,
    algo: &str,
    digest_hex: &str,
) -> String {
    if recursive && algo == "sha256" {
        return make_store_path(store_dir, "source", digest_hex, name);
    }
    let prefix = if recursive { "r:" } else { "" };
    let inner = Sha256::digest(format!("fixed:out:{prefix}{algo}:{digest_hex}:").as_bytes());
    make_store_path(store_dir, "output:out", &hex_lower(&inner), name)
}

pub fn hash_placeholder(output_name: &str) -> String {
    let sum = Sha256::digest(format!("nix-output:{output_name}").as_bytes());
    format!("/{}", nix_base32(&sum))
}

pub fn downstream_placeholder(drv_path: &str, output_name: &str) -> String {
    let base = Path::new(drv_path)
        .file_name()
        .and_then(|s| s.to_str())
        .expect("invalid drv path");
    let hash_part = &base[..32];
    let drv_name = base[33..].strip_suffix(".drv").expect("expected .drv");
    let out_path_name = if output_name == "out" {
        drv_name.to_owned()
    } else {
        format!("{drv_name}-{output_name}")
    };
    let sum = Sha256::digest(format!("nix-upstream-output:{hash_part}:{out_path_name}").as_bytes());
    format!("/{}", nix_base32(&sum))
}

#[derive(Clone, Debug)]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
    pub hash_algo: String,
    pub hash: String,
}

#[derive(Clone, Debug)]
pub struct Derivation {
    pub outputs: Vec<DerivationOutput>,
    pub input_drvs: BTreeMap<String, Vec<String>>,
    pub input_srcs: BTreeSet<String>,
    pub platform: String,
    pub builder: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

fn write_aterm_str(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for b in s.bytes() {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

impl Derivation {
    pub fn to_aterm(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(b"Derive([");
        let mut outputs = self.outputs.clone();
        outputs.sort_by(|a, b| a.name.cmp(&b.name));
        for (i, o) in outputs.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            out.push(b'(');
            write_aterm_str(&mut out, &o.name);
            out.push(b',');
            write_aterm_str(&mut out, &o.path);
            out.push(b',');
            write_aterm_str(&mut out, &o.hash_algo);
            out.push(b',');
            write_aterm_str(&mut out, &o.hash);
            out.push(b')');
        }
        out.extend_from_slice(b"],[");
        for (i, (drv_path, outs)) in self.input_drvs.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            out.push(b'(');
            write_aterm_str(&mut out, drv_path);
            out.extend_from_slice(b",[");
            let mut sorted_outs: Vec<&str> = outs.iter().map(String::as_str).collect();
            sorted_outs.sort_unstable();
            sorted_outs.dedup();
            for (j, o) in sorted_outs.iter().enumerate() {
                if j > 0 {
                    out.push(b',');
                }
                write_aterm_str(&mut out, o);
            }
            out.extend_from_slice(b"])");
        }
        out.extend_from_slice(b"],[");
        for (i, src) in self.input_srcs.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            write_aterm_str(&mut out, src);
        }
        out.extend_from_slice(b"],");
        write_aterm_str(&mut out, &self.platform);
        out.push(b',');
        write_aterm_str(&mut out, &self.builder);
        out.extend_from_slice(b",[");
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            write_aterm_str(&mut out, arg);
        }
        out.extend_from_slice(b"],[");
        for (i, (k, v)) in self.env.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            out.push(b'(');
            write_aterm_str(&mut out, k);
            out.push(b',');
            write_aterm_str(&mut out, v);
            out.push(b')');
        }
        out.extend_from_slice(b"])");
        out
    }

    pub fn references(&self) -> Vec<String> {
        let mut refs: BTreeSet<String> = self.input_srcs.clone();
        for drv in self.input_drvs.keys() {
            refs.insert(drv.clone());
        }
        refs.into_iter().collect()
    }
}

pub fn make_fetchurl_drv(store_dir: &str, name: &str, url: &str, sha256_hex: &str) -> (String, String, Vec<u8>, Vec<String>) {
    let out_path = make_fixed_output_path(store_dir, name, false, "sha256", sha256_hex);
    let mut env = BTreeMap::new();
    env.insert("builder".into(), "builtin:fetchurl".into());
    env.insert("executable".into(), String::new());
    env.insert(
        "impureEnvVars".into(),
        "http_proxy https_proxy ftp_proxy all_proxy no_proxy".into(),
    );
    env.insert("name".into(), name.into());
    env.insert("out".into(), out_path.clone());
    env.insert("outputHash".into(), sha256_hex.into());
    env.insert("outputHashAlgo".into(), "sha256".into());
    env.insert("outputHashMode".into(), "flat".into());
    env.insert("preferLocalBuild".into(), "1".into());
    env.insert("system".into(), "builtin".into());
    env.insert("unpack".into(), String::new());
    env.insert("url".into(), url.into());
    env.insert("urls".into(), url.into());

    let drv = Derivation {
        outputs: vec![DerivationOutput {
            name: "out".into(),
            path: out_path.clone(),
            hash_algo: "sha256".into(),
            hash: sha256_hex.into(),
        }],
        input_drvs: BTreeMap::new(),
        input_srcs: BTreeSet::new(),
        platform: "builtin".into(),
        builder: "builtin:fetchurl".into(),
        args: Vec::new(),
        env,
    };
    let aterm = drv.to_aterm();
    let refs = drv.references();
    let drv_path = make_text_path(store_dir, &format!("{name}.drv"), &aterm, &refs);
    (drv_path, out_path, aterm, refs)
}

pub struct TextNode {
    pub path: String,
    pub name: String,
    pub content: Vec<u8>,
    pub refs: Vec<String>,
}

// --- Nix Daemon Protocol 1.21 Client ---

pub struct DaemonClient {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

fn parse_socket_path(remote: &str) -> Result<String> {
    if remote.is_empty() {
        return Err("NIX_REMOTE is empty".into());
    }
    if let Some(rest) = remote.strip_prefix("unix://") {
        let path = rest.split('?').next().unwrap_or(rest);
        if path.is_empty() {
            return Err(format!("empty socket path in NIX_REMOTE={remote:?}"));
        }
        return Ok(path.to_owned());
    }
    if remote == "daemon" {
        return Ok("/nix/var/nix/daemon-socket/socket".into());
    }
    Err(format!("unsupported NIX_REMOTE={remote:?}"))
}

impl DaemonClient {
    pub fn connect(remote: &str) -> Result<Self> {
        let sock = parse_socket_path(remote)?;
        let stream = UnixStream::connect(&sock).map_err(|e| format!("connecting to {sock}: {e}"))?;
        let reader_stream = stream.try_clone().map_err(|e| format!("clone socket: {e}"))?;
        let mut client = Self {
            reader: BufReader::new(reader_stream),
            writer: BufWriter::new(stream),
        };
        client.handshake()?;
        Ok(client)
    }

    fn write_u64(&mut self, v: u64) -> Result<()> {
        self.writer
            .write_all(&v.to_le_bytes())
            .map_err(|e| format!("daemon write u64: {e}"))
    }

    fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
        self.write_u64(data.len() as u64)?;
        self.writer
            .write_all(data)
            .map_err(|e| format!("daemon write bytes: {e}"))?;
        let pad = (8 - (data.len() % 8)) % 8;
        if pad > 0 {
            self.writer
                .write_all(&[0u8; 8][..pad])
                .map_err(|e| format!("daemon write pad: {e}"))?;
        }
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> Result<()> {
        self.write_bytes(s.as_bytes())
    }

    fn write_string_set(&mut self, items: &[String]) -> Result<()> {
        self.write_u64(items.len() as u64)?;
        for s in items {
            self.write_str(s)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(|e| format!("daemon flush: {e}"))
    }

    fn read_u64(&mut self) -> Result<u64> {
        let mut buf = [0u8; 8];
        self.reader
            .read_exact(&mut buf)
            .map_err(|e| format!("daemon read u64: {e}"))?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_bytes(&mut self) -> Result<Vec<u8>> {
        let n = self.read_u64()? as usize;
        if n > 256 * 1024 * 1024 {
            return Err(format!("daemon response too large: {n}"));
        }
        let mut buf = vec![0u8; n];
        self.reader
            .read_exact(&mut buf)
            .map_err(|e| format!("daemon read bytes: {e}"))?;
        let pad = (8 - (n % 8)) % 8;
        if pad > 0 {
            let mut junk = [0u8; 8];
            self.reader
                .read_exact(&mut junk[..pad])
                .map_err(|e| format!("daemon read pad: {e}"))?;
        }
        Ok(buf)
    }

    fn read_string(&mut self) -> Result<String> {
        let b = self.read_bytes()?;
        String::from_utf8(b).map_err(|e| format!("daemon invalid utf8: {e}"))
    }

    fn read_string_set(&mut self) -> Result<Vec<String>> {
        let n = self.read_u64()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.read_string()?);
        }
        Ok(out)
    }

    fn drain_stderr(&mut self) -> Result<()> {
        loop {
            match self.read_u64()? {
                STDERR_LAST => return Ok(()),
                STDERR_NEXT => {
                    let _ = self.read_bytes()?;
                }
                STDERR_ERROR => {
                    let msg = self.read_string()?;
                    let _ = self.read_u64()?;
                    return Err(format!("nix daemon error: {msg}"));
                }
                other => return Err(format!("unexpected nix daemon stderr code {other:#x}")),
            }
        }
    }

    fn handshake(&mut self) -> Result<()> {
        self.write_u64(CLIENT_MAGIC)?;
        self.flush()?;
        let magic = self.read_u64()?;
        if magic != SERVER_MAGIC {
            return Err(format!("unexpected nix daemon magic {magic:#x}"));
        }
        let _server_version = self.read_u64()?;
        self.write_u64(PROTOCOL_1_21)?;
        self.write_u64(0)?; // cpuAffinity
        self.write_u64(0)?; // reserveSpace
        self.flush()?;
        self.drain_stderr()
    }

    pub fn query_valid_paths(&mut self, paths: &[String]) -> Result<HashSet<String>> {
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        self.write_u64(WOP_QUERY_VALID_PATHS)?;
        self.write_string_set(paths)?;
        self.flush()?;
        self.drain_stderr()?;
        let valid = self.read_string_set()?;
        Ok(valid.into_iter().collect())
    }

    pub fn add_text_to_store(&mut self, name: &str, text: &[u8], refs: &[String]) -> Result<String> {
        self.write_u64(WOP_ADD_TEXT_TO_STORE)?;
        self.write_str(name)?;
        self.write_bytes(text)?;
        self.write_string_set(refs)?;
        self.flush()?;
        self.drain_stderr()?;
        self.read_string()
    }
}

pub fn register_graph(remote: &str, nodes: &[TextNode]) -> Result<()> {
    if nodes.is_empty() {
        return Ok(());
    }
    let mut probe = DaemonClient::connect(remote)?;
    let all_paths: Vec<String> = nodes.iter().map(|n| n.path.clone()).collect();
    let valid = probe.query_valid_paths(&all_paths)?;
    let missing_indices: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| !valid.contains(&n.path))
        .map(|(i, _)| i)
        .collect();
    if missing_indices.is_empty() {
        return Ok(());
    }
    if missing_indices.len() == 1 {
        let n = &nodes[missing_indices[0]];
        let got = probe.add_text_to_store(&n.name, &n.content, &n.refs)?;
        if got != n.path {
            return Err(format!("daemon returned {got}, expected {}", n.path));
        }
        return Ok(());
    }
    drop(probe);

    let mut idx_by_path: HashMap<&str, usize> = HashMap::new();
    for &i in &missing_indices {
        idx_by_path.insert(nodes[i].path.as_str(), i);
    }
    let mut indeg: HashMap<usize, usize> = HashMap::new();
    let mut dependents: HashMap<usize, Vec<usize>> = HashMap::new();
    for &i in &missing_indices {
        let mut deps = 0usize;
        for r in &nodes[i].refs {
            if let Some(&dep_i) = idx_by_path.get(r.as_str()) {
                deps += 1;
                dependents.entry(dep_i).or_default().push(i);
            }
        }
        indeg.insert(i, deps);
    }
    let mut ready: VecDeque<usize> = missing_indices
        .iter()
        .copied()
        .filter(|i| indeg[i] == 0)
        .collect();

    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8)
        .min(missing_indices.len());
    let mut conns = Vec::with_capacity(workers);
    for _ in 0..workers {
        conns.push(DaemonClient::connect(remote)?);
    }

    let mut remaining = missing_indices.len();
    while remaining > 0 {
        if ready.is_empty() {
            return Err("cycle in derivation registration graph".into());
        }
        let batch: Vec<usize> = ready.drain(..).collect();
        let queue = Arc::new(Mutex::new(VecDeque::from(batch.clone())));
        let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        thread::scope(|s| {
            for conn in &mut conns {
                let queue = Arc::clone(&queue);
                let error = Arc::clone(&error);
                s.spawn(move || loop {
                    if error.lock().unwrap().is_some() {
                        break;
                    }
                    let next = queue.lock().unwrap().pop_front();
                    let Some(idx) = next else { break };
                    let n = &nodes[idx];
                    match conn.add_text_to_store(&n.name, &n.content, &n.refs) {
                        Ok(got) if got == n.path => {}
                        Ok(got) => {
                            *error.lock().unwrap() =
                                Some(format!("daemon returned {got}, expected {}", n.path));
                            break;
                        }
                        Err(e) => {
                            *error.lock().unwrap() = Some(e);
                            break;
                        }
                    }
                });
            }
        });
        if let Some(err) = error.lock().unwrap().take() {
            return Err(err);
        }
        remaining -= batch.len();
        for idx in batch {
            if let Some(children) = dependents.get(&idx) {
                for &child in children {
                    let d = indeg.get_mut(&child).unwrap();
                    *d -= 1;
                    if *d == 0 {
                        ready.push_back(child);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetchurl_hex_0_4_3_matches_nix() {
        let (drv_path, out_path, _, _) = make_fetchurl_drv(
            "/nix/store",
            "crate-hex-0.4.3.tar.gz",
            "https://crates.io/api/v1/crates/hex/0.4.3/download",
            "7f24254aa9a54b5c858eaee2f5bccdb46aaf0e486a595ed5fd8f86ba55232a70",
        );
        assert_eq!(
            out_path,
            "/nix/store/bwlrr0al7g1c2dn2xbhd6d9qrbjdhkh9-crate-hex-0.4.3.tar.gz"
        );
        assert_eq!(
            drv_path,
            "/nix/store/77b1l1zdzrdssl7cw7cgn6ycpy6r6pf6-crate-hex-0.4.3.tar.gz.drv"
        );
    }
}
