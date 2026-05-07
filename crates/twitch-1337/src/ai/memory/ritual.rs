//! Daily dreamer ritual. Spawned from `run_bot`; sleeps until [ai.dreamer].run_at,
//! rotates the transcript, and runs the dreamer LLM against every memory file
//! plus yesterday's transcript inside nonce-fenced blocks.

use std::sync::Arc;
use std::time::Duration;

use chrono::{NaiveDate, NaiveTime, TimeZone as _};
use chrono_tz::Europe::Berlin;
use eyre::Result;
use llm::{AgentOpts, AgentOutcome, LlmClient, Message, ToolChatCompletionRequest, run_agent};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::ai::memory::inject::{
    BuildOpts, FenceLabel, InvocationChannel, SubstitutionVars, build_chat_turn_context,
    fence_block, fresh_nonce, scrub_for_inject, substitute,
};
use crate::ai::memory::store::MemoryStore;
use crate::ai::memory::tools::{DreamerExecutor, DreamerExecutorOpts, dreamer_tools};
use crate::ai::memory::transcript::TranscriptWriter;

pub struct RitualConfig {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub run_at: NaiveTime,
    pub timeout_secs: u64,
    pub max_rounds: usize,
    pub max_writes_per_turn: usize,
    pub inject_byte_budget: usize,
    pub channel: String,
}

pub fn spawn_ritual(
    llm: Arc<dyn LlmClient>,
    store: MemoryStore,
    transcript: TranscriptWriter,
    cfg: RitualConfig,
    shutdown: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let now = chrono::Utc::now().with_timezone(&Berlin);
            let target = Berlin
                .from_local_datetime(&now.date_naive().and_time(cfg.run_at))
                .single();
            let next = target.filter(|t| *t > now).unwrap_or_else(|| {
                let tomorrow = now.date_naive().succ_opt().unwrap();
                Berlin
                    .from_local_datetime(&tomorrow.and_time(cfg.run_at))
                    .single()
                    .expect("valid")
            });
            let wait = (next - now).to_std().unwrap_or_default();
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = shutdown.notified() => return,
            }
            let yesterday = next.date_naive().pred_opt().expect("valid prev day");
            if let Err(e) = run_ritual(&*llm, &store, &transcript, &cfg, yesterday).await {
                warn!(error = ?e, "dreamer ritual failed");
            }
        }
    })
}

pub async fn run_ritual(
    llm: &dyn LlmClient,
    store: &MemoryStore,
    transcript: &TranscriptWriter,
    cfg: &RitualConfig,
    rotate_to: NaiveDate,
) -> Result<()> {
    let dated = transcript.rotate_to(rotate_to).await?;
    let transcript_text = tokio::fs::read_to_string(&dated).await.unwrap_or_default();
    let nonce = fresh_nonce();

    let mem_ctx = build_chat_turn_context(
        store,
        BuildOpts {
            inject_byte_budget: cfg.inject_byte_budget,
            nonce: nonce.clone(),
            primary_history: None,
            primary_login: String::new(),
            ai_channel_history: None,
            ai_channel_login: None,
            invocation_channel: InvocationChannel::Primary,
        },
    )
    .await?;
    let date_str = rotate_to.format("%Y-%m-%d").to_string();
    let transcript_block = fence_block(
        FenceLabel::Transcript { date: &date_str },
        &nonce,
        &scrub_for_inject(&transcript_text),
    );

    let dreamer_template =
        tokio::fs::read_to_string(store.prompts_dir().join("dreamer.md")).await?;
    let now_str = chrono::Utc::now()
        .with_timezone(&Berlin)
        .format("%Y-%m-%d")
        .to_string();
    let head = substitute(
        &dreamer_template,
        SubstitutionVars {
            speaker_username: "dreamer",
            speaker_role: "dreamer",
            channel: &cfg.channel,
            date: &now_str,
        },
    );
    let mem_block = mem_ctx.memory;
    let system_prompt = format!("{head}\n\n{mem_block}\n{transcript_block}");

    let exec = DreamerExecutor::new(DreamerExecutorOpts {
        store: store.clone(),
        max_writes_per_turn: cfg.max_writes_per_turn,
    });

    let req = ToolChatCompletionRequest {
        model: cfg.model.clone(),
        messages: vec![Message::system(system_prompt), Message::user("revise.")],
        tools: dreamer_tools(),
        reasoning_effort: cfg.reasoning_effort.clone(),
        prior_rounds: Vec::new(),
    };
    let opts = AgentOpts {
        max_rounds: cfg.max_rounds,
        per_round_timeout: Some(Duration::from_secs(cfg.timeout_secs)),
    };
    match run_agent(llm, req, &exec, opts).await {
        Ok(AgentOutcome::Text(_)) => info!(rotated = %dated.display(), "dreamer ritual finished"),
        Ok(AgentOutcome::MaxRoundsExceeded) => warn!("dreamer max_rounds reached"),
        Ok(AgentOutcome::Timeout { round }) => warn!(round, "dreamer per-round timeout"),
        Err(e) => warn!(error = ?e, "dreamer llm error"),
    }
    Ok(())
}
