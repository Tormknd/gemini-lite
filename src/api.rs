use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::sse;

pub const API_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

const SYSTEM_PROMPT: &str = "You are a highly efficient, expert AI assistant. \
    Be direct, concise, and prioritize accuracy. If the user asks a coding question, \
    provide senior-level answers without unnecessary fluff. \
    Respond in French unless asked otherwise.";

// ── Request types (sent to Gemini) ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    pub role: String,
    pub parts: Vec<Part>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Content>,
    contents: &'a [Content],
}

// ── Response types (received from Gemini, strictly typed) ───────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamResponse {
    pub candidates: Option<Vec<Candidate>>,
    pub usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct Candidate {
    pub content: Option<CandidateContent>,
}

#[derive(Debug, Deserialize)]
pub struct CandidateContent {
    pub parts: Vec<ResponsePart>,
}

#[derive(Debug, Deserialize)]
pub struct ResponsePart {
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    pub total_token_count: Option<u32>,
}

// ── UI event channel ────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum UiEvent {
    Delta(String),
    Done(String, u32),
    Error(String),
}

// ── Extraction helpers ──────────────────────────────────────────────────────

pub fn extract_text_and_tokens(resp: &StreamResponse) -> (String, Option<u32>) {
    let mut out = String::new();
    if let Some(candidates) = &resp.candidates {
        for candidate in candidates {
            if let Some(content) = &candidate.content {
                for part in &content.parts {
                    if let Some(text) = &part.text {
                        out.push_str(text);
                    }
                }
            }
        }
    }
    let tokens = resp
        .usage_metadata
        .as_ref()
        .and_then(|m| m.total_token_count);
    (out, tokens)
}

// ── Streaming call ──────────────────────────────────────────────────────────

pub async fn stream_gemini(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model_id: &str,
    history: &[Content],
    tx: &async_channel::Sender<UiEvent>,
) -> Result<(String, u32)> {
    let system_instruction = Some(Content {
        role: "user".to_string(),
        parts: vec![Part {
            text: SYSTEM_PROMPT.to_string(),
        }],
    });

    let body = GenerateRequest {
        system_instruction,
        contents: history,
    };

    let resp = client
        .post(format!(
            "{base_url}/{model_id}:streamGenerateContent?alt=sse&key={api_key}"
        ))
        .json(&body)
        .send()
        .await
        .context("HTTP request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let json: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse API error response")?;
        let msg = json["error"]["message"]
            .as_str()
            .unwrap_or("unknown API error");
        anyhow::bail!("API {status}: {msg}");
    }

    let mut stream = resp.bytes_stream();
    let mut sse_buffer = String::new();
    let mut residual = Vec::new();
    let mut full_text = String::new();
    let mut last_snapshot = String::new();
    let mut final_tokens = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("failed to read stream chunk")?;
        sse::append_chunk(&mut sse_buffer, &mut residual, &chunk);

        for event in sse::extract_events(&mut sse_buffer) {
            if event == "[DONE]" {
                continue;
            }
            let payload: StreamResponse =
                serde_json::from_str(&event).context("invalid SSE JSON payload")?;
            let (fragment, tokens) = extract_text_and_tokens(&payload);
            if let Some(t) = tokens {
                final_tokens = t;
            }
            if !fragment.is_empty() {
                let delta = if fragment.starts_with(&last_snapshot) {
                    fragment[last_snapshot.len()..].to_string()
                } else {
                    fragment.clone()
                };
                last_snapshot = fragment;
                if delta.is_empty() {
                    continue;
                }
                full_text.push_str(&delta);
                tx.send(UiEvent::Delta(delta))
                    .await
                    .context("failed to send UI delta")?;
            }
        }
    }

    if !sse_buffer.trim().is_empty() {
        sse_buffer.push_str("\n\n");
        for event in sse::extract_events(&mut sse_buffer) {
            if event == "[DONE]" {
                continue;
            }
            let payload: StreamResponse =
                serde_json::from_str(&event).context("invalid trailing SSE JSON payload")?;
            let (fragment, tokens) = extract_text_and_tokens(&payload);
            if let Some(t) = tokens {
                final_tokens = t;
            }
            if !fragment.is_empty() {
                let delta = if fragment.starts_with(&last_snapshot) {
                    fragment[last_snapshot.len()..].to_string()
                } else {
                    fragment.clone()
                };
                last_snapshot = fragment;
                if delta.is_empty() {
                    continue;
                }
                full_text.push_str(&delta);
                tx.send(UiEvent::Delta(delta))
                    .await
                    .context("failed to send trailing UI delta")?;
            }
        }
    }

    if full_text.is_empty() {
        anyhow::bail!("stream ended without model text");
    }

    Ok((full_text, final_tokens))
}
