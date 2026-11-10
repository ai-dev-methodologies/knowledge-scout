// Vendored from vercel-labs/agent-browser (Apache-2.0), cli/src/native/cdp/types.rs at commit
// e7519957b9067c541825afa21a792fbe1f483047. See NOTICE and third_party/agent-browser/LICENSE.
// Modifications: kept only the CDP message envelope types and the generated-protocol module;
// removed the hand-written domain result types this crate does not use.
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// CDP message envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpCommand {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CdpMessage {
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<CdpError>,
    pub method: Option<String>,
    pub params: Option<Value>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CdpError {
    pub code: Option<i64>,
    pub message: String,
    pub data: Option<String>,
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

// ---------------------------------------------------------------------------
// CDP events (broadcast to subscribers)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

/// Auto-generated CDP protocol types from the JSON protocol definitions (build.rs).
/// Usage: `use crate::cdp::types::generated::cdp_page::*;`
#[allow(clippy::all)]
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/cdp_generated.rs"));
}
