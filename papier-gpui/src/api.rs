//! The papier cloud client: the same VM the web viewer and the iPad talk to
//! (http://<vm>:8000 over the tailnet). /papier/api/* is the service,
//! /papier/data|inbound/* are static document assets. Every call here is
//! blocking; callers run them on background threads.

use std::{io::Read, sync::OnceLock, time::Duration};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::model::{Doc, DocState, InkPage, InkPatch, Library, patch_json};

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(6))
            .timeout(Duration::from_secs(25))
            .user_agent("papier-gpui")
            .build()
    })
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn body(resp: ureq::Response) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    resp.into_reader().take(256 << 20).read_to_end(&mut buf)?;
    Ok(buf)
}

#[derive(Clone)]
pub struct Client {
    /// e.g. "http://100.90.235.68:8000" — no trailing slash.
    pub root: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PiEvent {
    pub id: i64,
    /// turn | patch | seq | goto | notice
    #[serde(rename = "type")]
    pub kind: String,
    pub state: Option<String>,
    pub page: Option<i64>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PiState {
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub font: String,
    pub epoch: Option<f64>,
    #[serde(default)]
    pub events: Vec<PiEvent>,
}

pub enum Fetched {
    Fresh(Library, Option<String>),
    NotModified,
}

impl Client {
    pub fn new(root: &str) -> Self {
        Self { root: normalize(root) }
    }

    fn api(&self, path: &str) -> String {
        format!("{}/papier/api{}", self.root, path)
    }

    pub fn library(&self, etag: Option<&str>) -> Result<Fetched> {
        let mut req = agent().get(&self.api("/library"));
        if let Some(tag) = etag {
            req = req.set("If-None-Match", tag);
        }
        let resp = req.call().map_err(describe)?;
        if resp.status() == 304 {
            return Ok(Fetched::NotModified);
        }
        let tag = resp.header("ETag").map(str::to_owned);
        let lib: Library = serde_json::from_slice(&body(resp)?)?;
        Ok(Fetched::Fresh(lib, tag))
    }

    /// Absolute URL of a server path ("/papier/api/cover?...").
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.root, path)
    }

    /// Pre-rendered raster for a pdf page, immutable per doc version.
    pub fn page_url(&self, doc: &Doc, pdf_page: u32) -> String {
        format!("{}{}docs/{}/pages/{:04}.png?v={}", self.root, doc.base, doc.id, pdf_page + 1, doc.version)
    }

    /// The server's raw render of one source-PDF page (0-based), the same
    /// pages the web viewer draws with PDF.js.
    pub fn preview_url(&self, doc: &Doc, pdf_page: u32) -> String {
        format!("{}/papier/api/preview?id={}&page={pdf_page}&v={}", self.root, enc(&doc.id), doc.src_version.as_deref().unwrap_or(""))
    }

    /// The book's original PDF (what the web viewer renders with PDF.js).
    pub fn source_pdf_url(&self, doc: &Doc) -> String {
        format!("{}/papier/api/source-pdf?id={}&v={}", self.root, enc(&doc.id), doc.src_version.as_deref().unwrap_or(""))
    }

    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        body(agent().get(url).call().map_err(describe)?)
    }

    /// A page's ink from the MERGED truth (inbound overlay, where iPad, web
    /// and cloud-pi writes land, falling back to the tablet mirror).
    pub fn fetch_ink(&self, doc_id: &str, key: &str) -> Result<Option<InkPage>> {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
        let url = self.api(&format!("/ink?id={}&file={}.json&t={t}", enc(doc_id), enc(key)));
        match agent().get(&url).set("Cache-Control", "no-cache").call() {
            Ok(resp) => Ok(InkPage::parse(&body(resp)?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    fn post(&self, path: &str, data: &[u8]) -> Result<Vec<u8>> {
        let resp = agent().post(&self.api(path)).set("Content-Type", "application/json").send_bytes(data).map_err(describe)?;
        body(resp)
    }

    pub fn post_ink(&self, doc_id: &str, key: &str, page: &InkPage) -> Result<()> {
        self.post(&format!("/ink?id={}&file={}.json", enc(doc_id), enc(key)), &page.serialized()).map(|_| ())
    }

    pub fn post_state(&self, doc_id: &str, state: &DocState) -> Result<()> {
        self.post(&format!("/state?id={}", enc(doc_id)), &state.serialized()).map(|_| ())
    }

    pub fn erase_patch(&self, doc_id: &str, key: &str, patch: u64) -> Result<()> {
        self.post(&format!("/patch-erase?id={}&file={}.json&patch={patch}", enc(doc_id), enc(key)), b"").map(|_| ())
    }

    pub fn replace_patch(&self, doc_id: &str, key: &str, patch: &InkPatch, next_stroke: u64) -> Result<()> {
        let payload = serde_json::json!({ "patch": patch_json(patch), "next_stroke": next_stroke });
        self.post(
            &format!("/patch-replace?id={}&file={}.json&patch={}", enc(doc_id), enc(key), patch.id),
            &serde_json::to_vec(&payload)?,
        )
        .map(|_| ())
    }

    pub fn create_notebook(&self, title: &str) -> Result<String> {
        let out = self.post("/notebook", &serde_json::to_vec(&serde_json::json!({ "title": title }))?)?;
        let v: serde_json::Value = serde_json::from_slice(&out)?;
        v.get("id").and_then(|i| i.as_str()).map(str::to_owned).ok_or_else(|| anyhow!("notebook create failed"))
    }

    pub fn delete_document(&self, doc_id: &str) -> Result<()> {
        self.post(&format!("/delete?id={}", enc(doc_id)), b"").map(|_| ())
    }

    /// The remote pi session service. GET for events, POST for the rest.
    pub fn pi(&self, path_and_query: &str, post: bool) -> Result<PiState> {
        let url = self.api(&format!("/pi/{path_and_query}"));
        let req = if post { agent().post(&url) } else { agent().get(&url) }.timeout(Duration::from_secs(15));
        let resp = if post { req.send_bytes(b"") } else { req.call() }.map_err(describe)?;
        let data = body(resp)?;
        let state: PiState = serde_json::from_slice(&data)?;
        Ok(state)
    }
}

pub fn normalize(root: &str) -> String {
    let mut s = root.trim().to_owned();
    while s.ends_with('/') {
        s.pop();
    }
    s
}

pub fn pi_query(doc_id: &str) -> String {
    format!("id={}", enc(doc_id))
}

fn describe(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let text = resp.into_string().unwrap_or_default();
            anyhow!("HTTP {code} {}", text.chars().take(120).collect::<String>())
        }
        ureq::Error::Transport(t) => anyhow!("{}", t.message().map(str::to_owned).unwrap_or_else(|| t.kind().to_string())),
    }
}

#[allow(dead_code)]
pub fn check(root: &str) -> Result<usize> {
    match Client::new(root).library(None)? {
        Fetched::Fresh(lib, _) => Ok(lib.docs.len()),
        Fetched::NotModified => bail!("unexpected 304"),
    }
}
