//! P7.4 decision 4 / desktop foundation A17: the EmbedderHost owns one
//! killable helper process per local-ORT role (text + CLIP) and converges them
//! on the runtime plan exactly like
//! `apply_supervisor_plan` does, on the same 2 s converge loop
//! (`state.rs::PLAN_CONVERGE_INTERVAL`).
//!
//! ORT never loads in the desktop process. The installed executable re-enters
//! through `--photoproof-embedder-helper`; that child constructs the native
//! sessions and then serves bounded binary inference RPCs over stdio. A plan
//! change, timeout, retry, or shutdown can therefore kill and reap a
//! wedged constructor without risking the journal process.
//!
//! Each role has an independent monitor lane. A hung CLIP construction cannot
//! serialize or starve text construction (or a replacement CLIP generation).
//! Readiness means the helper acknowledged construction and remains the live
//! session owner; inference is proxied to that exact process.
//!
//! FAILURE: a native load failure (corrupt weights, missing file, an ort
//! shape error) marks the role `Failed(msg)` — visible in the debug panel
//! (`debug_lines`), surfaced as idle/degraded in the settings row text —
//! and NEVER crashes the app. RUNTIME §3.3's whole defense ("a crash inside
//! ort crashes Photoproof") rests on this isolation being honest: a load
//! that fails leaves the journal and every other feature untouched.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use photoproof_connectors::config::{EmbedderBackend, TextEmbedderBackend};
use photoproof_connectors::model_specs::{is_known_clip_model, is_known_text_model};
use photoproof_connectors::{
    ConnectorError, ConnectorResult, DecodedImage, Embedder, Embedding, ExecutionSelection,
    ModelExecution, SessionExecution,
};
use photoproof_core::UtcMillis;
use photoproof_core::runtime::plan::{ProcessPlan, RuntimePlan};

use crate::dto::{EmbedderSlot, EmbedderState};

const DEFAULT_BUILD_TIMEOUT: Duration = Duration::from_secs(180);
const HELPER_FLAG: &str = "--photoproof-embedder-helper";
const FRAME_MAGIC: [u8; 4] = *b"PPE1";
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: usize = MAX_FRAME_BYTES - 8;
const MAX_MODEL_ID_BYTES: usize = 512;
const MAX_VECTOR_DIMS: usize = 65_536;
const OP_EMBED_TEXT: u8 = 1;
const OP_EMBED_IMAGE: u8 = 2;
const RESP_READY: u8 = 16;
const RESP_EMBEDDING: u8 = 17;
const RESP_ERROR: u8 = 18;
const RESP_EXECUTION: u8 = 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Text,
    Clip,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Clip => "clip",
        }
    }
}

#[derive(Debug, Clone)]
struct Attempt {
    id: u64,
    model_id: String,
    generation: u64,
    started_epoch_ms: i64,
    started_mono: Duration,
}

impl Attempt {
    fn started_at(&self) -> String {
        UtcMillis::from_epoch_ms(self.started_epoch_ms).to_rfc3339()
    }
}

/// One role's explicit lifecycle. A role becomes `Queued` before dispatch so
/// status never reports Idle while a native build is pending. It becomes
/// `Building` when its independent role worker enters helper construction.
enum Slot {
    Idle,
    Queued {
        attempt: Attempt,
    },
    Building {
        attempt: Attempt,
    },
    Ready {
        attempt: Attempt,
        embedder: Arc<EmbedderProxy>,
    },
    Failed {
        attempt: Attempt,
        msg: String,
    },
    /// Shutdown invalidated every landing generation and is killing/reaping
    /// the role helper. This is honest terminal process state until exit.
    Stopping {
        attempt: Option<Attempt>,
    },
}

impl Slot {
    fn planned_model(&self) -> Option<&str> {
        match self {
            Slot::Idle => None,
            Slot::Queued { attempt }
            | Slot::Building { attempt }
            | Slot::Ready { attempt, .. }
            | Slot::Failed { attempt, .. } => Some(&attempt.model_id),
            Slot::Stopping { attempt } => attempt.as_ref().map(|attempt| attempt.model_id.as_str()),
        }
    }

    fn attempt(&self) -> Option<&Attempt> {
        match self {
            Slot::Idle => None,
            Slot::Queued { attempt }
            | Slot::Building { attempt }
            | Slot::Ready { attempt, .. }
            | Slot::Failed { attempt, .. } => Some(attempt),
            Slot::Stopping { attempt } => attempt.as_ref(),
        }
    }

    fn to_dto(&self, idle_generation: u64) -> EmbedderSlot {
        let attempt = self.attempt();
        EmbedderSlot {
            state: match self {
                Slot::Idle => EmbedderState::Idle,
                Slot::Queued { .. } => EmbedderState::Queued,
                Slot::Building { .. } => EmbedderState::Building,
                Slot::Ready { .. } => EmbedderState::Ready,
                Slot::Failed { .. } => EmbedderState::Failed,
                Slot::Stopping { .. } => EmbedderState::Stopping,
            },
            attempt_id: attempt.map(|attempt| attempt.id),
            model_id: attempt.map(|attempt| attempt.model_id.clone()),
            generation: attempt
                .map(|attempt| attempt.generation)
                .unwrap_or(idle_generation),
            started_at: attempt.map(Attempt::started_at),
            error: match self {
                Slot::Failed { msg, .. } => Some(msg.clone()),
                _ => None,
            },
            execution: match self {
                Slot::Ready { embedder, .. } => Some(embedder.execution()),
                _ => None,
            },
        }
    }
}

struct BuiltHelper {
    process: Arc<HelperProcess>,
    model_id: String,
    dims: usize,
    execution: ModelExecution,
}

trait Builder: Send + Sync {
    fn is_known(&self, role: Role, model_id: &str) -> bool;
    fn build(
        &self,
        role: Role,
        model_id: &str,
        models_dir: &Path,
        publish: &mut dyn FnMut(Arc<HelperProcess>) -> bool,
    ) -> ConnectorResult<BuiltHelper>;
}

struct ProcessBuilder;

impl Builder for ProcessBuilder {
    fn is_known(&self, role: Role, model_id: &str) -> bool {
        match role {
            Role::Text => is_known_text_model(model_id),
            Role::Clip => is_known_clip_model(model_id),
        }
    }

    fn build(
        &self,
        role: Role,
        model_id: &str,
        models_dir: &Path,
        publish: &mut dyn FnMut(Arc<HelperProcess>) -> bool,
    ) -> ConnectorResult<BuiltHelper> {
        let process = HelperProcess::spawn(role, model_id, models_dir)?;
        if !publish(Arc::clone(&process)) {
            process.terminate();
            return Err(ConnectorError::Cancelled);
        }
        match process.read_ready() {
            Ok(ready) => Ok(BuiltHelper {
                process,
                model_id: ready.model_id,
                dims: ready.dims,
                execution: ready.execution,
            }),
            Err(error) => {
                process.terminate();
                Err(error)
            }
        }
    }
}

#[derive(Debug)]
struct ReadyFrame {
    model_id: String,
    dims: usize,
    execution: ModelExecution,
}

struct HelperProcess {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<ChildStdout>>,
    rpc: Mutex<()>,
    terminated: AtomicBool,
}

type HelperRegistry = BTreeMap<(Role, u64), Arc<HelperProcess>>;

impl HelperProcess {
    fn spawn(role: Role, model_id: &str, models_dir: &Path) -> ConnectorResult<Arc<Self>> {
        validate_helper_args(role, model_id, models_dir)?;
        let executable = std::env::current_exe().map_err(ConnectorError::ConnectionLost)?;
        let mut command = Command::new(executable);
        command
            .arg(HELPER_FLAG)
            .arg(role.label())
            .arg(model_id)
            .arg(models_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        Self::spawn_command(command)
    }

    fn spawn_command(mut command: Command) -> ConnectorResult<Arc<Self>> {
        let mut child = command.spawn().map_err(ConnectorError::ConnectionLost)?;
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ConnectorError::ConnectionLost(io::Error::other(
                "embedder helper stdin unavailable",
            )));
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(ConnectorError::ConnectionLost(io::Error::other(
                "embedder helper stdout unavailable",
            )));
        };
        Ok(Arc::new(Self {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            stdout: Mutex::new(Some(stdout)),
            rpc: Mutex::new(()),
            terminated: AtomicBool::new(false),
        }))
    }

    fn read_ready(&self) -> ConnectorResult<ReadyFrame> {
        let (kind, body) = self.read_response()?;
        match kind {
            RESP_READY => decode_ready(&body),
            RESP_ERROR => Err(ConnectorError::Backend {
                status: 500,
                message: decode_error(&body)?,
            }),
            other => Err(ConnectorError::Decode(format!(
                "expected helper ready frame, got opcode {other}"
            ))),
        }
    }

    fn embed_text(
        &self,
        text: &str,
        execution: &Mutex<ModelExecution>,
    ) -> ConnectorResult<Embedding> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(ConnectorError::Backend {
                status: 413,
                message: "embedder text request exceeds protocol limit".into(),
            });
        }
        self.rpc(OP_EMBED_TEXT, text.as_bytes(), execution)
    }

    fn embed_image(
        &self,
        image: &DecodedImage,
        execution: &Mutex<ModelExecution>,
    ) -> ConnectorResult<Embedding> {
        let expected = (image.width as usize)
            .checked_mul(image.height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| ConnectorError::Decode("image dimensions overflow".into()))?;
        if image.width == 0
            || image.height == 0
            || expected != image.rgb8.len()
            || expected > MAX_IMAGE_BYTES
        {
            return Err(ConnectorError::Backend {
                status: 422,
                message: "invalid embedder image payload".into(),
            });
        }
        let mut body = Vec::with_capacity(8 + image.rgb8.len());
        body.extend_from_slice(&image.width.to_le_bytes());
        body.extend_from_slice(&image.height.to_le_bytes());
        body.extend_from_slice(&image.rgb8);
        self.rpc(OP_EMBED_IMAGE, &body, execution)
    }

    fn rpc(
        &self,
        opcode: u8,
        body: &[u8],
        execution: &Mutex<ModelExecution>,
    ) -> ConnectorResult<Embedding> {
        let _rpc = self.rpc.lock().expect("embedder helper rpc mutex");
        if self.terminated.load(Ordering::Acquire) {
            return Err(ConnectorError::Cancelled);
        }
        {
            let mut stdin = self.stdin.lock().expect("embedder helper stdin");
            let Some(stdin) = stdin.as_mut() else {
                return Err(ConnectorError::Cancelled);
            };
            write_frame(&mut *stdin, opcode, body).map_err(ConnectorError::ConnectionLost)?;
            stdin.flush().map_err(ConnectorError::ConnectionLost)?;
        }
        // One bounded execution-status update may precede the terminal
        // embedding/error frame. The helper sends it after each inference so
        // first-run profile evidence becomes committed status before this RPC
        // returns to the scheduler.
        for _ in 0..2 {
            let (kind, body) = self.read_response()?;
            match kind {
                RESP_EXECUTION => {
                    let update = decode_ready(&body)?.execution;
                    let mut current = execution.lock().expect("embedder execution mutex");
                    if update.model_id != current.model_id {
                        return Err(ConnectorError::Decode(
                            "helper execution update changed model identity".into(),
                        ));
                    }
                    *current = update;
                }
                RESP_EMBEDDING => return decode_embedding(&body),
                RESP_ERROR => {
                    return Err(ConnectorError::Backend {
                        status: 500,
                        message: decode_error(&body)?,
                    });
                }
                other => {
                    return Err(ConnectorError::Decode(format!(
                        "unexpected helper response opcode {other}"
                    )));
                }
            }
        }
        Err(ConnectorError::Decode(
            "helper execution update had no terminal response".into(),
        ))
    }

    fn read_response(&self) -> ConnectorResult<(u8, Vec<u8>)> {
        let mut stdout = self.stdout.lock().expect("embedder helper stdout");
        let Some(stdout) = stdout.as_mut() else {
            return Err(ConnectorError::Cancelled);
        };
        read_frame(stdout).map_err(ConnectorError::ConnectionLost)
    }

    #[cfg(test)]
    fn read_ready_after_test_harness_preamble(&self) -> ConnectorResult<ReadyFrame> {
        let mut stdout = self.stdout.lock().expect("embedder helper stdout");
        let Some(stdout) = stdout.as_mut() else {
            return Err(ConnectorError::Cancelled);
        };
        let mut matched = 0_usize;
        for _ in 0..4096 {
            let mut byte = [0_u8; 1];
            stdout
                .read_exact(&mut byte)
                .map_err(ConnectorError::ConnectionLost)?;
            if byte[0] == FRAME_MAGIC[matched] {
                matched += 1;
                if matched == FRAME_MAGIC.len() {
                    let mut tail = [0_u8; 5];
                    stdout
                        .read_exact(&mut tail)
                        .map_err(ConnectorError::ConnectionLost)?;
                    let len =
                        u32::from_le_bytes(tail[1..5].try_into().expect("frame length")) as usize;
                    if len > MAX_FRAME_BYTES {
                        return Err(ConnectorError::Decode(
                            "fixture ready frame exceeds limit".into(),
                        ));
                    }
                    let mut body = vec![0_u8; len];
                    stdout
                        .read_exact(&mut body)
                        .map_err(ConnectorError::ConnectionLost)?;
                    return match tail[0] {
                        RESP_READY => decode_ready(&body),
                        RESP_ERROR => Err(ConnectorError::Backend {
                            status: 500,
                            message: decode_error(&body)?,
                        }),
                        other => Err(ConnectorError::Decode(format!(
                            "unexpected fixture ready opcode {other}"
                        ))),
                    };
                }
            } else {
                matched = usize::from(byte[0] == FRAME_MAGIC[0]);
            }
        }
        Err(ConnectorError::Decode(
            "fixture helper never emitted protocol magic".into(),
        ))
    }

    #[cfg(test)]
    fn is_reaped(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
            && self.child.lock().expect("embedder helper child").is_none()
    }

    fn terminate(&self) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        // Closing the request pipe helps a healthy helper exit; kill remains
        // the bounded path for a native constructor/inference call that is not
        // reading stdin. Do not acquire the RPC/stdout locks: a caller may be
        // blocked in read_response and must be interrupted by child death.
        self.stdin.lock().expect("embedder helper stdin").take();
        let mut child = self.child.lock().expect("embedder helper child");
        if let Some(mut child) = child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub struct EmbedderProxy {
    model_id: String,
    dims: usize,
    execution: Mutex<ModelExecution>,
    process: Arc<HelperProcess>,
    attempt: Attempt,
    slot: Weak<Mutex<Slot>>,
    generation: Weak<AtomicU64>,
}

impl EmbedderProxy {
    pub fn runs_on_accelerator(&self) -> bool {
        let execution = self.execution.lock().expect("embedder execution mutex");
        !execution.sessions.is_empty()
            && execution
                .sessions
                .iter()
                .all(SessionExecution::is_proven_accelerated)
    }

    fn execution(&self) -> ModelExecution {
        self.execution
            .lock()
            .expect("embedder execution mutex")
            .clone()
    }

    fn is_current(&self) -> bool {
        let (Some(slot), Some(generation)) = (self.slot.upgrade(), self.generation.upgrade())
        else {
            return false;
        };
        generation.load(Ordering::Acquire) == self.attempt.generation
            && matches!(
                &*slot.lock().expect("embedder slot"),
                Slot::Ready { attempt, .. } if attempt.id == self.attempt.id
            )
    }

    fn fail_current(&self, error: &ConnectorError) {
        let (Some(slot), Some(generation)) = (self.slot.upgrade(), self.generation.upgrade())
        else {
            return;
        };
        fail_attempt_if_current(
            &slot,
            &generation,
            &self.attempt,
            format!("embedder helper failed: {error}"),
        );
        self.process.terminate();
    }
}

impl Embedder for EmbedderProxy {
    async fn embed_text(&self, text: &str) -> ConnectorResult<Embedding> {
        if !self.is_current() {
            return Err(ConnectorError::Cancelled);
        }
        let result = self.process.embed_text(text, &self.execution);
        if let Err(error) = &result {
            self.fail_current(error);
        }
        if result.is_ok() && !self.is_current() {
            return Err(ConnectorError::Cancelled);
        }
        result
    }

    async fn embed_image(&self, image: &DecodedImage) -> ConnectorResult<Embedding> {
        if !self.is_current() {
            return Err(ConnectorError::Cancelled);
        }
        let result = self.process.embed_image(image, &self.execution);
        if let Err(error) = &result {
            self.fail_current(error);
        }
        if result.is_ok() && !self.is_current() {
            return Err(ConnectorError::Cancelled);
        }
        result
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadyWire {
    model_id: String,
    dims: usize,
    sessions: Vec<SessionWire>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionWire {
    requested: Vec<String>,
    available: Vec<String>,
    registered: Vec<String>,
    selected: String,
    #[serde(default)]
    actual: Vec<String>,
    fallback_reason: Option<String>,
    measurement: String,
    profile_path: Option<String>,
}

fn validate_helper_args(role: Role, model_id: &str, models_dir: &Path) -> ConnectorResult<()> {
    let known = match role {
        Role::Text => is_known_text_model(model_id),
        Role::Clip => is_known_clip_model(model_id),
    };
    if !known
        || model_id.is_empty()
        || model_id.len() > MAX_MODEL_ID_BYTES
        || !model_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(ConnectorError::Backend {
            status: 400,
            message: "invalid embedder helper model id".into(),
        });
    }
    if !models_dir.is_absolute() || models_dir.as_os_str().len() > 32_768 {
        return Err(ConnectorError::Backend {
            status: 400,
            message: "invalid embedder helper models directory".into(),
        });
    }
    Ok(())
}

fn write_frame(mut writer: impl Write, opcode: u8, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "embedder helper frame exceeds limit",
        ));
    }
    writer.write_all(&FRAME_MAGIC)?;
    writer.write_all(&[opcode])?;
    writer.write_all(&(body.len() as u32).to_le_bytes())?;
    writer.write_all(body)
}

fn read_frame(mut reader: impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0_u8; 9];
    reader.read_exact(&mut header)?;
    if header[..4] != FRAME_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "embedder helper protocol magic/version mismatch",
        ));
    }
    let len = u32::from_le_bytes(header[5..9].try_into().expect("four-byte frame length")) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "embedder helper frame exceeds limit",
        ));
    }
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body)?;
    Ok((header[4], body))
}

fn decode_ready(body: &[u8]) -> ConnectorResult<ReadyFrame> {
    let wire: ReadyWire =
        serde_json::from_slice(body).map_err(|error| ConnectorError::Decode(error.to_string()))?;
    if wire.model_id.is_empty()
        || wire.model_id.len() > MAX_MODEL_ID_BYTES
        || wire.dims == 0
        || wire.dims > MAX_VECTOR_DIMS
        || wire.sessions.len() > 4
    {
        return Err(ConnectorError::Decode(
            "invalid embedder helper ready payload".into(),
        ));
    }
    let sessions = wire
        .sessions
        .into_iter()
        .map(|session| {
            let selected = match session.selected.as_str() {
                "cpu" => ExecutionSelection::Cpu,
                "core-ml" => ExecutionSelection::CoreMl,
                "cuda" => ExecutionSelection::Cuda,
                "tensor-rt" => ExecutionSelection::TensorRt,
                "unknown" => ExecutionSelection::Unknown,
                other => {
                    return Err(ConnectorError::Decode(format!(
                        "unknown helper execution selection {other}"
                    )));
                }
            };
            let measurement: &'static str = match session.measurement.as_str() {
                "configured" => "configured",
                "pending-profile" => "pending-profile",
                "profiled" => "profiled",
                "profile-unavailable" => "profile-unavailable",
                "unknown" => "unknown",
                other => {
                    return Err(ConnectorError::Decode(format!(
                        "unknown helper execution measurement {other}"
                    )));
                }
            };
            if session
                .fallback_reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 2_048)
                || session
                    .profile_path
                    .as_ref()
                    .is_some_and(|path| path.len() > 32_768)
            {
                return Err(ConnectorError::Decode(
                    "helper execution detail exceeds protocol limit".into(),
                ));
            }
            Ok(SessionExecution {
                requested: validate_provider_list("requested", session.requested)?,
                available: validate_provider_list("available", session.available)?,
                registered: validate_provider_list("registered", session.registered)?,
                selected,
                actual: validate_provider_list("actual", session.actual)?,
                fallback_reason: session.fallback_reason,
                measurement,
                profile_path: session.profile_path,
            })
        })
        .collect::<ConnectorResult<Vec<_>>>()?;
    Ok(ReadyFrame {
        model_id: wire.model_id.clone(),
        dims: wire.dims,
        execution: ModelExecution {
            model_id: wire.model_id,
            sessions,
        },
    })
}

fn validate_provider_list(label: &str, providers: Vec<String>) -> ConnectorResult<Vec<String>> {
    if providers.len() > 8
        || providers
            .iter()
            .any(|provider| !matches!(provider.as_str(), "CPU" | "CoreML" | "CUDA" | "TensorRT"))
    {
        return Err(ConnectorError::Decode(format!(
            "invalid helper {label} provider list"
        )));
    }
    Ok(providers)
}

fn decode_embedding(body: &[u8]) -> ConnectorResult<Embedding> {
    if body.len() < 6 {
        return Err(ConnectorError::Decode(
            "short embedder helper embedding frame".into(),
        ));
    }
    let model_len = u16::from_le_bytes(body[..2].try_into().expect("model length")) as usize;
    if model_len == 0 || model_len > MAX_MODEL_ID_BYTES || body.len() < 6 + model_len {
        return Err(ConnectorError::Decode(
            "invalid embedder helper model length".into(),
        ));
    }
    let model_id = std::str::from_utf8(&body[2..2 + model_len])
        .map_err(|error| ConnectorError::Decode(error.to_string()))?
        .to_owned();
    let count_offset = 2 + model_len;
    let count = u32::from_le_bytes(
        body[count_offset..count_offset + 4]
            .try_into()
            .expect("vector length"),
    ) as usize;
    let expected = count_offset
        .checked_add(4)
        .and_then(|offset| {
            count
                .checked_mul(4)
                .and_then(|bytes| offset.checked_add(bytes))
        })
        .ok_or_else(|| ConnectorError::Decode("embedding length overflow".into()))?;
    if count == 0 || count > MAX_VECTOR_DIMS || body.len() != expected {
        return Err(ConnectorError::Decode(
            "invalid embedder helper vector length".into(),
        ));
    }
    let vector = body[count_offset + 4..]
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte float")))
        .collect();
    Ok(Embedding { vector, model_id })
}

fn decode_error(body: &[u8]) -> ConnectorResult<String> {
    if body.len() > MAX_TEXT_BYTES {
        return Err(ConnectorError::Decode(
            "embedder helper error exceeds limit".into(),
        ));
    }
    std::str::from_utf8(body)
        .map(str::to_owned)
        .map_err(|error| ConnectorError::Decode(error.to_string()))
}

trait Dispatcher: Send + Sync {
    fn dispatch(
        &self,
        thread_name: &'static str,
        job: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Option<JoinHandle<()>>>;
}

struct ThreadDispatcher;

impl Dispatcher for ThreadDispatcher {
    fn dispatch(
        &self,
        thread_name: &'static str,
        job: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Option<JoinHandle<()>>> {
        std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(job)
            .map(Some)
    }
}

#[derive(Debug, Clone, Copy)]
struct ClockReading {
    epoch_ms: i64,
    monotonic: Duration,
}

trait Clock: Send + Sync {
    fn now(&self) -> ClockReading;
}

struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> ClockReading {
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        ClockReading {
            epoch_ms,
            monotonic: self.origin.elapsed(),
        }
    }
}

pub struct EmbedderHost {
    text: Arc<Mutex<Slot>>,
    text_gen: Arc<AtomicU64>,
    clip: Arc<Mutex<Slot>>,
    clip_gen: Arc<AtomicU64>,
    children: Arc<Mutex<HelperRegistry>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    stopped: Arc<AtomicBool>,
    next_attempt: Arc<AtomicU64>,
    builder: Arc<dyn Builder>,
    dispatcher: Arc<dyn Dispatcher>,
    clock: Arc<dyn Clock>,
    build_timeout: Duration,
}

pub struct ReadyVectorIdentities {
    pub text: Option<(String, u64)>,
    pub clip: Option<(String, u64)>,
}

impl EmbedderHost {
    pub fn new() -> Self {
        Self::with_dependencies(
            Arc::new(ProcessBuilder),
            Arc::new(ThreadDispatcher),
            Arc::new(SystemClock::new()),
            DEFAULT_BUILD_TIMEOUT,
        )
    }

    fn with_dependencies(
        builder: Arc<dyn Builder>,
        dispatcher: Arc<dyn Dispatcher>,
        clock: Arc<dyn Clock>,
        build_timeout: Duration,
    ) -> Self {
        Self {
            text: Arc::new(Mutex::new(Slot::Idle)),
            text_gen: Arc::new(AtomicU64::new(0)),
            clip: Arc::new(Mutex::new(Slot::Idle)),
            clip_gen: Arc::new(AtomicU64::new(0)),
            children: Arc::new(Mutex::new(BTreeMap::new())),
            workers: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(AtomicBool::new(false)),
            next_attempt: Arc::new(AtomicU64::new(1)),
            builder,
            dispatcher,
            clock,
            build_timeout,
        }
    }

    pub fn text_ready(&self) -> bool {
        self.expire_timed_out();
        matches!(&*self.text.lock().expect("text slot"), Slot::Ready { .. })
    }

    pub fn clip_ready(&self) -> bool {
        self.expire_timed_out();
        matches!(&*self.clip.lock().expect("clip slot"), Slot::Ready { .. })
    }

    pub fn text_slot(&self) -> EmbedderSlot {
        self.expire_timed_out();
        self.text
            .lock()
            .expect("text slot")
            .to_dto(self.text_gen.load(Ordering::SeqCst))
    }

    pub fn clip_slot(&self) -> EmbedderSlot {
        self.expire_timed_out();
        self.clip
            .lock()
            .expect("clip slot")
            .to_dto(self.clip_gen.load(Ordering::SeqCst))
    }

    pub fn text(&self) -> Option<Arc<EmbedderProxy>> {
        self.expire_timed_out();
        match &*self.text.lock().expect("text slot") {
            Slot::Ready { embedder, .. } => Some(embedder.clone()),
            _ => None,
        }
    }

    pub fn clip(&self) -> Option<Arc<EmbedderProxy>> {
        self.expire_timed_out();
        match &*self.clip.lock().expect("clip slot") {
            Slot::Ready { embedder, .. } => Some(embedder.clone()),
            _ => None,
        }
    }

    /// Identity of each actually-ready vector writer. The generation changes
    /// across unload/failure/retry even when the selected model id does not,
    /// which lets readiness-driven repair distinguish a genuine same-model
    /// reload from a repeated status observation.
    pub fn ready_vector_identities(&self) -> ReadyVectorIdentities {
        self.expire_timed_out();
        let text = match &*self.text.lock().expect("text slot") {
            Slot::Ready { attempt, .. } => Some((attempt.model_id.clone(), attempt.generation)),
            _ => None,
        };
        let clip = match &*self.clip.lock().expect("clip slot") {
            Slot::Ready { attempt, .. } => Some((attempt.model_id.clone(), attempt.generation)),
            _ => None,
        };
        ReadyVectorIdentities { text, clip }
    }

    /// True while either helper role still owns or is constructing this
    /// model. Removal first makes the model unavailable to the plan and
    /// converges both roles; only after this turns false may its files move.
    pub fn model_in_use(&self, model_id: &str) -> bool {
        self.expire_timed_out();
        [&self.text, &self.clip].into_iter().any(|slot| {
            slot.lock()
                .expect("embedder slot")
                .planned_model()
                .is_some_and(|current| current == model_id)
        })
    }

    /// Cancel every role that still references `model_id`. This is the
    /// synchronous unload seam used by serialized model removal: queued work
    /// is generation-invalidated, a published helper is killed and reaped,
    /// and the slot is Idle before this returns.
    pub fn cancel_model(&self, model_id: &str) {
        cancel_role_model(
            Role::Text,
            model_id,
            &self.text,
            &self.text_gen,
            &self.children,
        );
        cancel_role_model(
            Role::Clip,
            model_id,
            &self.clip,
            &self.clip_gen,
            &self.children,
        );
    }

    /// Debug-panel lines (§8.6): one per role, naming the state honestly so
    /// a degraded-with-error embedder is visible without a crash.
    pub fn debug_lines(&self) -> Vec<String> {
        self.expire_timed_out();
        vec![
            describe("text-embedder", &self.text.lock().expect("text slot")),
            describe("clip-embedder", &self.clip.lock().expect("clip slot")),
        ]
    }

    /// Converge both roles onto the plan, building/dropping as needed. The
    /// backends gate helper builds: only `local-ort` loads here (a
    /// remote/openai-compatible embedder is the runtime's HTTP seam, not
    /// this host's). Idempotent: a no-change plan touches nothing.
    pub fn apply(
        &self,
        plan: &RuntimePlan,
        clip_backend: EmbedderBackend,
        text_backend: TextEmbedderBackend,
        models_dir: &Path,
    ) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        self.expire_timed_out();
        // CLIP role. Only local-ort builds in a helper; an openai-compatible
        // embedder backend reaches a remote endpoint (not this host's job),
        // so it stays Idle here.
        let clip_target = match (&plan.clip_embedder, clip_backend) {
            (ProcessPlan::Run { model_id }, EmbedderBackend::LocalOrt) => Some(model_id.clone()),
            _ => None,
        };
        self.converge(
            Role::Clip,
            clip_target.as_deref(),
            models_dir,
            &self.clip,
            &self.clip_gen,
        );

        // Text role. local-ort builds here; local-llamacpp / openai-compatible
        // are remote seams (RUNTIME §3.3 alt backend) and stay Idle.
        let text_target = match (&plan.text_embedder, text_backend) {
            (ProcessPlan::Run { model_id }, TextEmbedderBackend::LocalOrt) => {
                Some(model_id.clone())
            }
            _ => None,
        };
        self.converge(
            Role::Text,
            text_target.as_deref(),
            models_dir,
            &self.text,
            &self.text_gen,
        );
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        terminate_helpers(take_all_helpers(&self.children));
        stop_slot(&self.text, &self.text_gen);
        stop_slot(&self.clip, &self.clip_gen);
        let workers = std::mem::take(&mut *self.workers.lock().expect("embedder workers"));
        for worker in workers {
            let _ = worker.join();
        }
    }

    pub fn retry_failed(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        reset_failed_slot(&self.text, &self.text_gen);
        reset_failed_slot(&self.clip, &self.clip_gen);
    }

    fn converge(
        &self,
        role: Role,
        target: Option<&str>,
        models_dir: &Path,
        slot: &Arc<Mutex<Slot>>,
        generation: &Arc<AtomicU64>,
    ) {
        let mut current = slot.lock().expect("embedder slot");
        if !needs_rebuild(&current, target) {
            return;
        }
        let next_generation = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let superseded = take_role_helpers(&self.children, role);
        let Some(model_id) = target else {
            *current = Slot::Idle;
            drop(current);
            terminate_helpers(superseded);
            return;
        };
        let reading = self.clock.now();
        let attempt = Attempt {
            id: self.next_attempt.fetch_add(1, Ordering::SeqCst),
            model_id: model_id.to_owned(),
            generation: next_generation,
            started_epoch_ms: reading.epoch_ms,
            started_mono: reading.monotonic,
        };
        if !self.builder.is_known(role, model_id) {
            *current = Slot::Failed {
                attempt,
                msg: format!("no ort {} recipe for {model_id}", role.label()),
            };
            return;
        }
        *current = Slot::Queued {
            attempt: attempt.clone(),
        };
        drop(current);
        terminate_helpers(superseded);

        let target_slot = Arc::clone(slot);
        let target_generation = Arc::clone(generation);
        let children = Arc::clone(&self.children);
        let builder = Arc::clone(&self.builder);
        let models_dir = models_dir.to_owned();
        let worker_attempt = attempt.clone();
        let thread_name = match role {
            Role::Text => "pp-embed-build-text",
            Role::Clip => "pp-embed-build-clip",
        };
        tracing::info!(
            role = role.label(),
            model = %attempt.model_id,
            attempt = attempt.id,
            generation = attempt.generation,
            "embedder build queued"
        );
        let dispatched = self.dispatcher.dispatch(
            thread_name,
            Box::new(move || {
                if !begin_build_if_current(&target_slot, &target_generation, &worker_attempt) {
                    return;
                }
                let mut publish = |process: Arc<HelperProcess>| {
                    publish_helper_if_current(
                        role,
                        &target_slot,
                        &target_generation,
                        &worker_attempt,
                        &children,
                        process,
                    )
                };
                let built =
                    builder.build(role, &worker_attempt.model_id, &models_dir, &mut publish);
                land_build(&target_slot, &target_generation, &worker_attempt, built);
            }),
        );
        match dispatched {
            Ok(Some(worker)) => self.workers.lock().expect("embedder workers").push(worker),
            Ok(None) => {}
            Err(error) => {
                fail_attempt_if_current(
                    slot,
                    generation,
                    &attempt,
                    format!("embedder build dispatch failed: {error}"),
                );
            }
        }
    }

    fn expire_timed_out(&self) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let now = self.clock.now().monotonic;
        expire_slot(
            Role::Text,
            &self.text,
            &self.text_gen,
            &self.children,
            now,
            self.build_timeout,
        );
        expire_slot(
            Role::Clip,
            &self.clip,
            &self.clip_gen,
            &self.children,
            now,
            self.build_timeout,
        );
    }
}

fn reset_failed_slot(slot: &Mutex<Slot>, generation: &AtomicU64) {
    let mut slot = slot.lock().expect("embedder slot");
    if matches!(&*slot, Slot::Failed { .. }) {
        generation.fetch_add(1, Ordering::SeqCst);
        *slot = Slot::Idle;
    }
}

fn stop_slot(slot: &Mutex<Slot>, generation: &AtomicU64) {
    generation.fetch_add(1, Ordering::SeqCst);
    let mut slot = slot.lock().expect("embedder slot");
    let attempt = slot.attempt().cloned();
    *slot = Slot::Stopping { attempt };
}

fn cancel_role_model(
    role: Role,
    model_id: &str,
    slot: &Mutex<Slot>,
    generation: &AtomicU64,
    children: &Mutex<HelperRegistry>,
) {
    let mut slot = slot.lock().expect("embedder slot");
    if slot.planned_model() != Some(model_id) {
        return;
    }
    generation.fetch_add(1, Ordering::SeqCst);
    *slot = Slot::Idle;
    let helpers = take_role_helpers(children, role);
    drop(slot);
    terminate_helpers(helpers);
}

impl Default for EmbedderHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a slot must rebuild for `target`. Rebuild iff the planned model
/// differs from the slot's current model (None target = drop to Idle). A
/// `Failed` slot for the SAME model does NOT rebuild — that would retry the
/// doomed load every converge tick; only a model/path change re-attempts.
fn needs_rebuild(slot: &Slot, target: Option<&str>) -> bool {
    slot.planned_model() != target
}

fn begin_build_if_current(slot: &Mutex<Slot>, generation: &AtomicU64, attempt: &Attempt) -> bool {
    if generation.load(Ordering::SeqCst) != attempt.generation {
        return false;
    }
    let mut slot = slot.lock().expect("embedder slot");
    if generation.load(Ordering::SeqCst) != attempt.generation
        || !matches!(
            &*slot,
            Slot::Queued { attempt: queued } if queued.id == attempt.id
        )
    {
        return false;
    }
    *slot = Slot::Building {
        attempt: attempt.clone(),
    };
    true
}

fn land_build(
    slot: &Arc<Mutex<Slot>>,
    generation: &Arc<AtomicU64>,
    attempt: &Attempt,
    built: ConnectorResult<BuiltHelper>,
) {
    if generation.load(Ordering::SeqCst) != attempt.generation {
        if let Ok(built) = built {
            built.process.terminate();
        }
        tracing::info!(
            model = %attempt.model_id,
            attempt = attempt.id,
            generation = attempt.generation,
            "embedder build superseded before land (discarded)"
        );
        return;
    }
    let mut guard = slot.lock().expect("embedder slot");
    if generation.load(Ordering::SeqCst) != attempt.generation
        || !matches!(
            &*guard,
            Slot::Building { attempt: building } if building.id == attempt.id
        )
    {
        drop(guard);
        if let Ok(built) = built {
            built.process.terminate();
        }
        return;
    }
    *guard = match built {
        Ok(built) => {
            tracing::info!(
                model = %attempt.model_id,
                attempt = attempt.id,
                "embedder build landed READY"
            );
            let embedder = EmbedderProxy {
                model_id: built.model_id,
                dims: built.dims,
                execution: Mutex::new(built.execution),
                process: built.process,
                attempt: attempt.clone(),
                slot: Arc::downgrade(slot),
                generation: Arc::downgrade(generation),
            };
            Slot::Ready {
                attempt: attempt.clone(),
                embedder: Arc::new(embedder),
            }
        }
        Err(e) => {
            tracing::warn!(
                model = %attempt.model_id,
                attempt = attempt.id,
                error = %e,
                "embedder build landed FAILED"
            );
            Slot::Failed {
                msg: format!("ort load failed: {e}"),
                attempt: attempt.clone(),
            }
        }
    };
}

fn fail_attempt_if_current(
    slot: &Mutex<Slot>,
    generation: &AtomicU64,
    attempt: &Attempt,
    msg: String,
) {
    if generation.load(Ordering::SeqCst) != attempt.generation {
        return;
    }
    let mut slot = slot.lock().expect("embedder slot");
    if generation.load(Ordering::SeqCst) != attempt.generation
        || !matches!(
            &*slot,
            Slot::Queued { attempt: current }
                | Slot::Building { attempt: current }
                | Slot::Ready { attempt: current, .. }
                if current.id == attempt.id
        )
    {
        return;
    }
    *slot = Slot::Failed {
        attempt: attempt.clone(),
        msg,
    };
}

fn expire_slot(
    role: Role,
    slot: &Mutex<Slot>,
    generation: &AtomicU64,
    children: &Mutex<HelperRegistry>,
    now: Duration,
    timeout: Duration,
) {
    let mut slot = slot.lock().expect("embedder slot");
    let attempt = match &*slot {
        Slot::Queued { attempt } | Slot::Building { attempt } => attempt.clone(),
        _ => return,
    };
    if now.saturating_sub(attempt.started_mono) < timeout {
        return;
    }
    generation.fetch_add(1, Ordering::SeqCst);
    let attempt_generation = attempt.generation;
    let msg = format!(
        "{} embedder build timed out after {}s; native build landing invalidated",
        role.label(),
        timeout.as_secs()
    );
    tracing::warn!(
        role = role.label(),
        model = %attempt.model_id,
        attempt = attempt.id,
        generation = attempt.generation,
        "embedder watchdog timed out native build"
    );
    *slot = Slot::Failed { attempt, msg };
    let helper = children
        .lock()
        .expect("embedder helper registry")
        .remove(&(role, attempt_generation));
    drop(slot);
    if let Some(helper) = helper {
        helper.terminate();
    }
}

fn publish_helper_if_current(
    role: Role,
    slot: &Mutex<Slot>,
    generation: &AtomicU64,
    attempt: &Attempt,
    children: &Mutex<HelperRegistry>,
    process: Arc<HelperProcess>,
) -> bool {
    if generation.load(Ordering::SeqCst) != attempt.generation {
        return false;
    }
    let slot = slot.lock().expect("embedder slot");
    if generation.load(Ordering::SeqCst) != attempt.generation
        || !matches!(
            &*slot,
            Slot::Building { attempt: building } if building.id == attempt.id
        )
    {
        return false;
    }
    children
        .lock()
        .expect("embedder helper registry")
        .insert((role, attempt.generation), process);
    true
}

fn take_role_helpers(children: &Mutex<HelperRegistry>, role: Role) -> Vec<Arc<HelperProcess>> {
    let mut children = children.lock().expect("embedder helper registry");
    let keys = children
        .keys()
        .filter(|(candidate, _)| *candidate == role)
        .copied()
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| children.remove(&key))
        .collect()
}

fn take_all_helpers(children: &Mutex<HelperRegistry>) -> Vec<Arc<HelperProcess>> {
    std::mem::take(&mut *children.lock().expect("embedder helper registry"))
        .into_values()
        .collect()
}

fn terminate_helpers(helpers: Vec<Arc<HelperProcess>>) {
    for helper in helpers {
        helper.terminate();
    }
}

fn describe(role: &str, slot: &Slot) -> String {
    match slot {
        Slot::Idle => format!("{role}: idle (no model)"),
        Slot::Queued { attempt } => format!(
            "{role}: queued {} (attempt {}, generation {})",
            attempt.model_id, attempt.id, attempt.generation
        ),
        Slot::Building { attempt } => format!(
            "{role}: building {} (attempt {}, generation {})",
            attempt.model_id, attempt.id, attempt.generation
        ),
        Slot::Ready { attempt, .. } => format!(
            "{role}: ready {} (attempt {}, generation {})",
            attempt.model_id, attempt.id, attempt.generation
        ),
        Slot::Failed { attempt, msg } => format!(
            "{role}: FAILED {} (attempt {}, generation {}) — {msg}",
            attempt.model_id, attempt.id, attempt.generation
        ),
        Slot::Stopping { attempt } => match attempt {
            Some(attempt) => format!(
                "{role}: stopping {} (attempt {}, generation {})",
                attempt.model_id, attempt.id, attempt.generation
            ),
            None => format!("{role}: stopping"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    fn run_plan(clip: Option<&str>, text: Option<&str>) -> RuntimePlan {
        let run = |id: Option<&str>| match id {
            Some(id) => ProcessPlan::Run {
                model_id: id.to_owned(),
            },
            None => ProcessPlan::NotConfigured {
                reason: "test".into(),
                fixable_by_download: false,
            },
        };
        RuntimePlan {
            effective_tier: 1,
            llm: run(None),
            asr: run(None),
            clip_embedder: run(clip),
            text_embedder: run(text),
        }
    }

    fn attempt(id: u64, model_id: &str, generation: u64) -> Attempt {
        Attempt {
            id,
            model_id: model_id.into(),
            generation,
            started_epoch_ms: 1_767_225_600_000,
            started_mono: Duration::ZERO,
        }
    }

    struct FakeClock {
        millis: AtomicU64,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                millis: AtomicU64::new(0),
            }
        }

        fn advance(&self, duration: Duration) {
            self.millis.fetch_add(
                duration.as_millis().min(u64::MAX as u128) as u64,
                Ordering::SeqCst,
            );
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> ClockReading {
            let millis = self.millis.load(Ordering::SeqCst);
            ClockReading {
                epoch_ms: 1_767_225_600_000 + millis as i64,
                monotonic: Duration::from_millis(millis),
            }
        }
    }

    struct CountingFailBuilder {
        calls: AtomicUsize,
    }

    impl CountingFailBuilder {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl Builder for CountingFailBuilder {
        fn is_known(&self, _role: Role, _model_id: &str) -> bool {
            true
        }

        fn build(
            &self,
            _role: Role,
            _model_id: &str,
            _models_dir: &Path,
            _publish: &mut dyn FnMut(Arc<HelperProcess>) -> bool,
        ) -> ConnectorResult<BuiltHelper> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(photoproof_connectors::ConnectorError::NotReady(
                "injected build failure",
            ))
        }
    }

    struct BlockingFailBuilder {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        calls: AtomicUsize,
        returned: AtomicBool,
    }

    impl Builder for BlockingFailBuilder {
        fn is_known(&self, _role: Role, _model_id: &str) -> bool {
            true
        }

        fn build(
            &self,
            _role: Role,
            _model_id: &str,
            _models_dir: &Path,
            _publish: &mut dyn FnMut(Arc<HelperProcess>) -> bool,
        ) -> ConnectorResult<BuiltHelper> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            self.returned.store(true, Ordering::SeqCst);
            Err(photoproof_connectors::ConnectorError::NotReady(
                "released injected build",
            ))
        }
    }

    type HeldJob = (&'static str, Box<dyn FnOnce() + Send + 'static>);

    struct HoldingDispatcher {
        jobs: Mutex<VecDeque<HeldJob>>,
        reject: Option<&'static str>,
    }

    impl HoldingDispatcher {
        fn new(reject: Option<&'static str>) -> Self {
            Self {
                jobs: Mutex::new(VecDeque::new()),
                reject,
            }
        }

        fn run(&self, name: &'static str) {
            let index = self
                .jobs
                .lock()
                .unwrap()
                .iter()
                .position(|(candidate, _)| *candidate == name)
                .expect("held job");
            let (_, job) = self.jobs.lock().unwrap().remove(index).unwrap();
            job();
        }
    }

    impl Dispatcher for HoldingDispatcher {
        fn dispatch(
            &self,
            thread_name: &'static str,
            job: Box<dyn FnOnce() + Send + 'static>,
        ) -> io::Result<Option<JoinHandle<()>>> {
            if self.reject == Some(thread_name) {
                return Err(io::Error::other("injected dispatch failure"));
            }
            self.jobs.lock().unwrap().push_back((thread_name, job));
            Ok(None)
        }
    }

    struct FixtureBuilder {
        modes: Mutex<BTreeMap<Role, VecDeque<String>>>,
        seen: Mutex<Vec<(Role, Arc<HelperProcess>)>>,
        marker: Option<std::path::PathBuf>,
    }

    impl FixtureBuilder {
        fn new(
            clip_modes: &[&str],
            text_modes: &[&str],
            marker: Option<std::path::PathBuf>,
        ) -> Self {
            Self {
                modes: Mutex::new(BTreeMap::from([
                    (
                        Role::Clip,
                        clip_modes.iter().map(|mode| (*mode).to_owned()).collect(),
                    ),
                    (
                        Role::Text,
                        text_modes.iter().map(|mode| (*mode).to_owned()).collect(),
                    ),
                ])),
                seen: Mutex::new(Vec::new()),
                marker,
            }
        }

        fn seen(&self) -> Vec<Arc<HelperProcess>> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .map(|(_, process)| Arc::clone(process))
                .collect()
        }

        fn seen_role(&self, role: Role) -> Vec<Arc<HelperProcess>> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|(candidate, _)| *candidate == role)
                .map(|(_, process)| Arc::clone(process))
                .collect()
        }
    }

    impl Builder for FixtureBuilder {
        fn is_known(&self, _role: Role, _model_id: &str) -> bool {
            true
        }

        fn build(
            &self,
            role: Role,
            model_id: &str,
            _models_dir: &Path,
            publish: &mut dyn FnMut(Arc<HelperProcess>) -> bool,
        ) -> ConnectorResult<BuiltHelper> {
            let mode = self
                .modes
                .lock()
                .unwrap()
                .get_mut(&role)
                .and_then(VecDeque::pop_front)
                .ok_or_else(|| ConnectorError::Decode("missing fixture helper mode".into()))?;
            let mut command = Command::new(std::env::current_exe().expect("test executable"));
            command
                .args([
                    "--ignored",
                    "--exact",
                    "embedders::tests::embedder_helper_process_fixture",
                    "--nocapture",
                ])
                .env("PHOTOPROOF_EMBEDDER_FIXTURE_MODE", mode)
                .env("PHOTOPROOF_EMBEDDER_FIXTURE_MODEL", model_id)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            if let Some(marker) = &self.marker {
                command.env("PHOTOPROOF_EMBEDDER_FIXTURE_MARKER", marker);
            }
            let process = HelperProcess::spawn_command(command)?;
            self.seen.lock().unwrap().push((role, Arc::clone(&process)));
            if !publish(Arc::clone(&process)) {
                process.terminate();
                return Err(ConnectorError::Cancelled);
            }
            match process.read_ready_after_test_harness_preamble() {
                Ok(ready) => Ok(BuiltHelper {
                    process,
                    model_id: ready.model_id,
                    dims: ready.dims,
                    execution: ready.execution,
                }),
                Err(error) => {
                    process.terminate();
                    Err(error)
                }
            }
        }
    }

    fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) {
        let deadline = Instant::now() + timeout;
        while !predicate() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(predicate(), "condition did not settle before {timeout:?}");
    }

    fn fixture_ready_body(model_id: &str) -> Vec<u8> {
        fixture_execution_body(model_id, Vec::new())
    }

    fn fixture_execution_body(model_id: &str, sessions: Vec<SessionWire>) -> Vec<u8> {
        serde_json::to_vec(&ReadyWire {
            model_id: model_id.to_owned(),
            dims: 3,
            sessions,
        })
        .unwrap()
    }

    fn fixture_session(actual: &[&str], fallback_reason: Option<&str>) -> SessionWire {
        SessionWire {
            requested: vec!["CUDA".into(), "CPU".into()],
            available: vec!["CUDA".into(), "CPU".into()],
            registered: vec!["CUDA".into(), "CPU".into()],
            selected: "unknown".into(),
            actual: actual
                .iter()
                .map(|provider| (*provider).to_owned())
                .collect(),
            fallback_reason: fallback_reason.map(str::to_owned),
            measurement: if actual.is_empty() {
                "pending-profile".into()
            } else {
                "profiled".into()
            },
            profile_path: None,
        }
    }

    fn fixture_embedding_body(model_id: &str) -> Vec<u8> {
        let vector = [0.1_f32, 0.2, 0.3];
        let mut body = Vec::with_capacity(6 + model_id.len() + vector.len() * 4);
        body.extend_from_slice(&(model_id.len() as u16).to_le_bytes());
        body.extend_from_slice(model_id.as_bytes());
        body.extend_from_slice(&(vector.len() as u32).to_le_bytes());
        for value in vector {
            body.extend_from_slice(&value.to_le_bytes());
        }
        body
    }

    #[test]
    #[ignore = "subprocess fixture for embedder helper supervision tests"]
    fn embedder_helper_process_fixture() {
        let mode = std::env::var("PHOTOPROOF_EMBEDDER_FIXTURE_MODE").unwrap();
        let model_id = std::env::var("PHOTOPROOF_EMBEDDER_FIXTURE_MODEL").unwrap();
        if mode == "hung-build" {
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        let mut stdout = std::io::stdout().lock();
        let initial = if mode == "profiled-fallback" {
            fixture_execution_body(&model_id, vec![fixture_session(&[], None)])
        } else {
            fixture_ready_body(&model_id)
        };
        write_frame(&mut stdout, RESP_READY, &initial).unwrap();
        stdout.flush().unwrap();
        if mode == "exit-after-ready" {
            return;
        }
        let mut stdin = std::io::stdin().lock();
        loop {
            let (opcode, body) = match read_frame(&mut stdin) {
                Ok(frame) => frame,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return,
                Err(error) => panic!("fixture request: {error}"),
            };
            if let Ok(marker) = std::env::var("PHOTOPROOF_EMBEDDER_FIXTURE_MARKER") {
                std::fs::write(marker, b"entered").unwrap();
            }
            if mode == "hung-inference" {
                std::thread::sleep(Duration::from_secs(30));
                return;
            }
            match opcode {
                OP_EMBED_TEXT => {
                    assert!(body.len() <= MAX_TEXT_BYTES);
                }
                OP_EMBED_IMAGE => {
                    assert!(body.len() >= 8);
                }
                other => panic!("unexpected fixture opcode {other}"),
            }
            if mode == "profiled-fallback" {
                write_frame(
                    &mut stdout,
                    RESP_EXECUTION,
                    &fixture_execution_body(
                        &model_id,
                        vec![fixture_session(
                            &["CPU"],
                            Some("provider profile proved complete CPU fallback"),
                        )],
                    ),
                )
                .unwrap();
            }
            write_frame(
                &mut stdout,
                RESP_EMBEDDING,
                &fixture_embedding_body(&model_id),
            )
            .unwrap();
            stdout.flush().unwrap();
        }
    }

    fn test_host(
        builder: Arc<dyn Builder>,
        dispatcher: Arc<dyn Dispatcher>,
        clock: Arc<dyn Clock>,
        timeout: Duration,
    ) -> EmbedderHost {
        EmbedderHost::with_dependencies(builder, dispatcher, clock, timeout)
    }

    /// A fresh host is fully degraded: neither role ready, both Idle in the
    /// debug lines. This is the shipping posture until weights install.
    #[test]
    fn fresh_host_is_degraded_and_idle() {
        let host = EmbedderHost::new();
        assert!(!host.text_ready());
        assert!(!host.clip_ready());
        assert!(host.text().is_none());
        assert!(host.clip().is_none());
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("text-embedder: idle"));
        assert!(lines.contains("clip-embedder: idle"));
    }

    /// The `Slot -> EmbedderSlot` projection carries lifecycle identity and
    /// an error only for Failed.
    /// rides ONLY on Failed; every other state carries None. This is the wire
    /// contract the settings rows read, so pin it directly off each variant.
    #[test]
    fn slot_to_dto_maps_lifecycle_and_attempt_identity() {
        assert_eq!(
            Slot::Idle.to_dto(7),
            EmbedderSlot {
                state: EmbedderState::Idle,
                attempt_id: None,
                model_id: None,
                generation: 7,
                started_at: None,
                error: None,
                execution: None,
            }
        );
        let active = attempt(4, "m", 8);
        assert_eq!(
            Slot::Building {
                attempt: active.clone(),
            }
            .to_dto(8),
            EmbedderSlot {
                state: EmbedderState::Building,
                attempt_id: Some(4),
                model_id: Some("m".into()),
                generation: 8,
                started_at: Some("2026-01-01T00:00:00.000Z".into()),
                error: None,
                execution: None,
            }
        );
        // Ready needs a real constructed connector; the fresh-host Ready path
        // is exercised by the ignored end-to-end test. Here pin the three
        // non-Ready projections plus the error-carrying Failed.
        let failed = Slot::Failed {
            attempt: active,
            msg: "ort load failed: boom".into(),
        }
        .to_dto(8);
        assert_eq!(failed.state, EmbedderState::Failed);
        assert_eq!(failed.error.as_deref(), Some("ort load failed: boom"));
    }

    /// A fresh host reports both slots Idle with no error — the same posture
    /// the bools report (false) but with the honest "inactive, not failed"
    /// distinction the bool cannot carry.
    #[test]
    fn fresh_host_slots_are_idle_no_error() {
        let host = EmbedderHost::new();
        assert_eq!(
            host.text_slot(),
            EmbedderSlot {
                state: EmbedderState::Idle,
                attempt_id: None,
                model_id: None,
                generation: 0,
                started_at: None,
                error: None,
                execution: None,
            }
        );
        assert_eq!(
            host.clip_slot(),
            EmbedderSlot {
                state: EmbedderState::Idle,
                attempt_id: None,
                model_id: None,
                generation: 0,
                started_at: None,
                error: None,
                execution: None,
            }
        );
    }

    #[test]
    fn dispatch_failure_is_terminal_failed_and_does_not_poison_other_role() {
        let builder = Arc::new(CountingFailBuilder::new());
        let dispatcher = Arc::new(HoldingDispatcher::new(Some("pp-embed-build-text")));
        let clock = Arc::new(FakeClock::new());
        let host = test_host(builder, dispatcher.clone(), clock, Duration::from_secs(10));

        host.apply(
            &run_plan(Some("clip-model"), Some("text-model")),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );

        let text = host.text_slot();
        assert_eq!(text.state, EmbedderState::Failed);
        assert!(text.error.as_deref().unwrap().contains("dispatch failed"));
        assert_eq!(text.model_id.as_deref(), Some("text-model"));
        assert_eq!(host.clip_slot().state, EmbedderState::Queued);

        dispatcher.run("pp-embed-build-clip");
        assert_eq!(host.clip_slot().state, EmbedderState::Failed);
        assert_eq!(host.text_slot().state, EmbedderState::Failed);
    }

    #[test]
    fn queued_attempt_times_out_and_late_job_never_calls_native_builder() {
        let builder = Arc::new(CountingFailBuilder::new());
        let dispatcher = Arc::new(HoldingDispatcher::new(None));
        let clock = Arc::new(FakeClock::new());
        let host = test_host(
            builder.clone(),
            dispatcher.clone(),
            clock.clone(),
            Duration::from_secs(5),
        );
        host.apply(
            &run_plan(Some("clip-model"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        let queued = host.clip_slot();
        assert_eq!(queued.state, EmbedderState::Queued);
        assert_eq!(queued.attempt_id, Some(1));
        assert_eq!(queued.model_id.as_deref(), Some("clip-model"));
        assert_eq!(queued.generation, 1);
        assert!(queued.started_at.is_some());

        clock.advance(Duration::from_secs(6));
        let failed = host.clip_slot();
        assert_eq!(failed.state, EmbedderState::Failed);
        assert!(failed.error.as_deref().unwrap().contains("timed out"));

        dispatcher.run("pp-embed-build-clip");
        assert_eq!(builder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(host.clip_slot().state, EmbedderState::Failed);
    }

    #[test]
    fn building_attempt_times_out_and_its_late_native_result_is_stale() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let builder = Arc::new(BlockingFailBuilder {
            entered: entered_tx,
            release: Mutex::new(release_rx),
            calls: AtomicUsize::new(0),
            returned: AtomicBool::new(false),
        });
        let clock = Arc::new(FakeClock::new());
        let host = test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            clock.clone(),
            Duration::from_secs(5),
        );
        host.apply(
            &run_plan(Some("clip-model"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("native build entered");
        assert_eq!(host.clip_slot().state, EmbedderState::Building);

        clock.advance(Duration::from_secs(6));
        assert_eq!(host.clip_slot().state, EmbedderState::Failed);
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !builder.returned.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(builder.returned.load(Ordering::SeqCst));
        assert_eq!(host.clip_slot().state, EmbedderState::Failed);
    }

    #[test]
    fn hung_helper_construction_is_killed_and_reaped_at_timeout() {
        let builder = Arc::new(FixtureBuilder::new(&["hung-build"], &[], None));
        let clock = Arc::new(FakeClock::new());
        let host = test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            clock.clone(),
            Duration::from_secs(5),
        );
        host.apply(
            &run_plan(Some("clip-fixture"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || !builder.seen().is_empty());
        assert_eq!(host.clip_slot().state, EmbedderState::Building);

        clock.advance(Duration::from_secs(6));
        let started = Instant::now();
        assert_eq!(host.clip_slot().state, EmbedderState::Failed);
        wait_until(Duration::from_secs(2), || {
            builder.seen_role(Role::Clip)[0].is_reaped()
        });
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout must synchronously kill and reap the native constructor"
        );
        host.shutdown();
    }

    #[test]
    fn serialized_removal_cancels_and_reaps_an_inflight_helper_build() {
        let builder = Arc::new(FixtureBuilder::new(&["hung-build"], &[], None));
        let host = test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            Arc::new(FakeClock::new()),
            Duration::from_secs(30),
        );
        host.apply(
            &run_plan(Some("clip-fixture"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || !builder.seen().is_empty());
        assert_eq!(host.clip_slot().state, EmbedderState::Building);

        let started = Instant::now();
        host.cancel_model("clip-fixture");

        assert_eq!(host.clip_slot().state, EmbedderState::Idle);
        assert!(!host.model_in_use("clip-fixture"));
        assert!(
            builder.seen_role(Role::Clip)[0].is_reaped(),
            "removal acknowledgement requires the helper to be reaped"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a killable helper build must not make removal wait for ORT"
        );
        host.shutdown();
    }

    #[test]
    fn quit_during_model_load_kills_and_reaps_the_helper_before_acknowledging() {
        let builder = Arc::new(FixtureBuilder::new(&["hung-build"], &[], None));
        let host = test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            Arc::new(FakeClock::new()),
            Duration::from_secs(30),
        );
        host.apply(
            &run_plan(Some("clip-fixture"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || !builder.seen().is_empty());
        assert_eq!(host.clip_slot().state, EmbedderState::Building);

        let started = Instant::now();
        host.shutdown();

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "quit must kill a helper blocked inside native model construction"
        );
        assert!(
            builder.seen_role(Role::Clip)[0].is_reaped(),
            "shutdown acknowledgement requires the model-load helper to be reaped"
        );
        assert_eq!(host.clip_slot().state, EmbedderState::Stopping);
    }

    #[test]
    fn hung_role_never_blocks_other_role_or_replacement_generation() {
        let builder = Arc::new(FixtureBuilder::new(
            &["hung-build", "ready"],
            &["ready"],
            None,
        ));
        let host = test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            Arc::new(FakeClock::new()),
            Duration::from_secs(30),
        );
        host.apply(
            &run_plan(Some("clip-fixture"), Some("text-fixture")),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || host.text_ready());
        assert_eq!(host.clip_slot().state, EmbedderState::Building);
        let text = host.text().expect("independent text helper ready");
        let embedded = pollster::block_on(text.embed_text("independent")).unwrap();
        assert_eq!(embedded.vector, [0.1, 0.2, 0.3]);

        host.apply(
            &run_plan(None, Some("text-fixture")),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || {
            builder.seen_role(Role::Clip)[0].is_reaped()
        });
        assert_eq!(host.clip_slot().state, EmbedderState::Idle);

        host.apply(
            &run_plan(Some("clip-fixture"), Some("text-fixture")),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || host.clip_ready());
        let clip = host.clip().expect("replacement helper ready");
        assert_eq!(
            pollster::block_on(clip.embed_text("replacement"))
                .unwrap()
                .vector,
            [0.1, 0.2, 0.3]
        );
        host.apply(
            &run_plan(None, Some("text-fixture")),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        assert!(matches!(
            pollster::block_on(clip.embed_text("stale generation")),
            Err(ConnectorError::Cancelled)
        ));
        host.shutdown();
        assert!(builder.seen().iter().all(|process| process.is_reaped()));
    }

    #[test]
    fn shutdown_kills_wedged_inference_and_reaps_before_acknowledging() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("inference-entered");
        let builder = Arc::new(FixtureBuilder::new(
            &["hung-inference"],
            &[],
            Some(marker.clone()),
        ));
        let host = Arc::new(test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            Arc::new(FakeClock::new()),
            Duration::from_secs(30),
        ));
        host.apply(
            &run_plan(Some("clip-fixture"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || host.clip_ready());
        let proxy = host.clip().unwrap();
        let inference = std::thread::spawn(move || {
            pollster::block_on(proxy.embed_text("wedge this inference"))
        });
        wait_until(Duration::from_secs(2), || marker.exists());

        let started = Instant::now();
        host.shutdown();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "quit must not wait for the native inference call"
        );
        assert!(inference.join().unwrap().is_err());
        assert!(builder.seen().iter().all(|process| process.is_reaped()));
        assert_eq!(host.clip_slot().state, EmbedderState::Stopping);
    }

    #[test]
    fn helper_crash_during_inference_flips_ready_to_failed() {
        let builder = Arc::new(FixtureBuilder::new(&["exit-after-ready"], &[], None));
        let host = test_host(
            builder.clone(),
            Arc::new(ThreadDispatcher),
            Arc::new(FakeClock::new()),
            Duration::from_secs(30),
        );
        host.apply(
            &run_plan(Some("clip-fixture"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || host.clip_ready());
        let proxy = host.clip().unwrap();
        assert!(pollster::block_on(proxy.embed_text("observe crash")).is_err());
        wait_until(Duration::from_secs(2), || {
            host.clip_slot().state == EmbedderState::Failed
        });
        assert!(
            host.clip_slot()
                .error
                .as_deref()
                .is_some_and(|error| error.contains("helper failed"))
        );
        host.shutdown();
        assert!(builder.seen().iter().all(|process| process.is_reaped()));
    }

    #[test]
    fn profiled_forced_fallback_updates_committed_status_before_rpc_returns() {
        let builder = Arc::new(FixtureBuilder::new(&["profiled-fallback"], &[], None));
        let host = test_host(
            builder,
            Arc::new(ThreadDispatcher),
            Arc::new(FakeClock::new()),
            Duration::from_secs(30),
        );
        host.apply(
            &run_plan(Some("clip-fixture"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        wait_until(Duration::from_secs(2), || host.clip_ready());
        let proxy = host.clip().unwrap();
        let pending = host.clip_slot().execution.unwrap();
        assert!(pending.sessions[0].actual.is_empty());
        assert_eq!(pending.sessions[0].selected, ExecutionSelection::Unknown);
        assert!(!proxy.runs_on_accelerator());

        pollster::block_on(proxy.embed_text("force fallback")).unwrap();

        let committed = host.clip_slot().execution.unwrap();
        assert_eq!(committed.sessions[0].actual, ["CPU"]);
        assert_eq!(committed.sessions[0].measurement, "profiled");
        assert_eq!(
            committed.sessions[0].fallback_reason.as_deref(),
            Some("provider profile proved complete CPU fallback")
        );
        assert!(
            !proxy.runs_on_accelerator(),
            "configured CUDA must not outrank profiled CPU truth"
        );
        host.shutdown();
    }

    #[test]
    fn protocol_rejects_oversized_frames_before_body_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&FRAME_MAGIC);
        bytes.push(OP_EMBED_TEXT);
        bytes.extend_from_slice(&((MAX_FRAME_BYTES + 1) as u32).to_le_bytes());
        let error = read_frame(std::io::Cursor::new(bytes)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn plan_drop_invalidates_a_queued_attempt_before_native_build() {
        let builder = Arc::new(CountingFailBuilder::new());
        let dispatcher = Arc::new(HoldingDispatcher::new(None));
        let host = test_host(
            builder.clone(),
            dispatcher.clone(),
            Arc::new(FakeClock::new()),
            Duration::from_secs(10),
        );
        host.apply(
            &run_plan(Some("clip-model"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        assert_eq!(host.clip_slot().state, EmbedderState::Queued);
        host.apply(
            &run_plan(None, None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        assert_eq!(host.clip_slot().state, EmbedderState::Idle);

        dispatcher.run("pp-embed-build-clip");
        assert_eq!(builder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(host.clip_slot().state, EmbedderState::Idle);
    }

    #[test]
    fn shutdown_is_terminal_stopping_and_invalidates_queued_jobs() {
        let builder = Arc::new(CountingFailBuilder::new());
        let dispatcher = Arc::new(HoldingDispatcher::new(None));
        let host = test_host(
            builder.clone(),
            dispatcher.clone(),
            Arc::new(FakeClock::new()),
            Duration::from_secs(10),
        );
        host.apply(
            &run_plan(Some("clip-model"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            Path::new("/models"),
        );
        host.shutdown();
        let stopped = host.clip_slot();
        assert_eq!(stopped.state, EmbedderState::Stopping);
        assert_eq!(stopped.model_id.as_deref(), Some("clip-model"));

        dispatcher.run("pp-embed-build-clip");
        assert_eq!(builder.calls.load(Ordering::SeqCst), 0);
        assert_eq!(host.clip_slot().state, EmbedderState::Stopping);
    }

    /// An unknown model id (no recipe) lands the slot in FAILED, not a
    /// panic — the synchronous resolve path is exercised without any model
    /// files. Both roles take the same firewall.
    #[test]
    fn unknown_model_id_fails_dark_never_panics() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");
        let plan = run_plan(Some("not-a-clip"), Some("not-a-text"));
        host.apply(
            &plan,
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        assert!(!host.text_ready());
        assert!(!host.clip_ready());
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("clip-embedder: FAILED"), "{lines}");
        assert!(lines.contains("text-embedder: FAILED"), "{lines}");
        // The slots report Failed with the recipe-mismatch message in `error`
        // (the host's synchronous "no ort recipe" landing) — so the UI shows a
        // failed row, not an eternal spinner.
        let clip = host.clip_slot();
        assert_eq!(clip.state, EmbedderState::Failed);
        assert_eq!(
            clip.error.as_deref(),
            Some("no ort clip recipe for not-a-clip")
        );
        let text = host.text_slot();
        assert_eq!(text.state, EmbedderState::Failed);
        assert_eq!(
            text.error.as_deref(),
            Some("no ort text recipe for not-a-text")
        );
    }

    /// A dark plan (NotConfigured everywhere) keeps both slots Idle, and a
    /// non-local-ort backend never builds a helper even when the plan
    /// would Run (the remote-endpoint seam stays the runtime's job).
    #[test]
    fn dark_plan_and_remote_backend_stay_idle() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");

        host.apply(
            &run_plan(None, None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        assert!(
            host.debug_lines()
                .join("\n")
                .contains("text-embedder: idle")
        );

        // Plan says Run, but the backend is openai-compatible: no helper
        // build — stays Idle.
        host.apply(
            &run_plan(
                Some("ViT-H-14-378-quickgelu__dfn5b"),
                Some("embeddinggemma-300m-q8"),
            ),
            EmbedderBackend::OpenaiCompatible,
            TextEmbedderBackend::OpenaiCompatible,
            dir,
        );
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("clip-embedder: idle"), "{lines}");
        assert!(lines.contains("text-embedder: idle"), "{lines}");
    }

    // The pinned id -> recipe/dims resolution moved to
    // `photoproof_connectors::model_specs` (so the eval rig shares it); its
    // `pinned_ids_resolve_to_recipes` test lives there now. This host's tests
    // below still pin the host BEHAVIOR (unknown id fails dark, drop discards a
    // stale build) through the delegated `is_known_*` / `build_*` seam.

    /// REGRESSION (review L4-host): a drop-to-Idle must discard a stale
    /// in-flight build. The Idle and Failed converge transitions bump the
    /// per-role generation, so a build dispatched before the drop fails the
    /// `land_build` gate and never overwrites the dropped slot with Ready —
    /// otherwise search and the embedding drain would run a model the plan
    /// has dropped (decision 4). We model the race without a real ort load:
    /// capture the dispatch generation, converge to a dropped plan (which
    /// must bump), then drive `land_build` with the captured generation and
    /// assert it is a no-op.
    #[test]
    fn drop_to_idle_discards_a_stale_inflight_build() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");

        // Tick 1: plan wants the CLIP model. `apply` dispatches a background
        // build and bumps clip_gen to its dispatch value. The build thread
        // will fail (no files) and try to land — but we race a drop first.
        let dispatch_gen = host.clip_gen.load(Ordering::SeqCst) + 1;
        host.apply(
            &run_plan(Some("ViT-H-14-378-quickgelu__dfn5b"), None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );

        // Tick 2: the user drops the plan (uninstall / backend swap). The
        // converge-to-Idle path must bump clip_gen so the dispatch_gen above
        // is now stale.
        host.apply(
            &run_plan(None, None),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        assert!(
            host.clip_gen.load(Ordering::SeqCst) > dispatch_gen,
            "drop-to-Idle must bump the generation past the in-flight dispatch"
        );

        // Now simulate the stale build finishing and trying to land against
        // the dropped slot. With the generation bumped, `land_build` returns
        // before touching the slot — so the slot stays Idle (NOT Failed,
        // which is what this Err would otherwise write), proving the stale
        // result never reached the slot at all. The Ready case is identical:
        // the gate is checked before the match on `built`.
        let stale_attempt = attempt(1, "ViT-H-14-378-quickgelu__dfn5b", dispatch_gen);
        land_build(
            &host.clip,
            &host.clip_gen,
            &stale_attempt,
            Err(photoproof_connectors::ConnectorError::NotReady("stale")),
        );
        assert!(!host.clip_ready(), "stale build must not land over a drop");
        assert!(
            host.debug_lines()
                .join("\n")
                .contains("clip-embedder: idle"),
            "stale Err must not even land Failed; slot must stay Idle: {}",
            host.debug_lines().join("\n")
        );
    }

    /// REGRESSION (review L4-host): after `shutdown()` the host is latched —
    /// a converge tick that fires during quit teardown (its flushes can
    /// exceed the 2 s interval) must NOT redispatch a fresh ort build. Here
    /// `apply` with a Run plan after shutdown is a no-op; the slot stays in
    /// honest terminal Stopping state.
    #[test]
    fn apply_after_shutdown_is_a_no_op() {
        let host = EmbedderHost::new();
        let dir = std::path::Path::new("/nonexistent/models");
        host.shutdown();
        host.apply(
            &run_plan(
                Some("ViT-H-14-378-quickgelu__dfn5b"),
                Some("embeddinggemma-300m-q8"),
            ),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            dir,
        );
        let lines = host.debug_lines().join("\n");
        assert!(lines.contains("clip-embedder: stopping"), "{lines}");
        assert!(lines.contains("text-embedder: stopping"), "{lines}");
    }

    #[test]
    fn explicit_restart_resets_only_failed_embedder_slots() {
        let host = EmbedderHost::new();
        {
            *host.text.lock().unwrap() = Slot::Failed {
                attempt: attempt(1, "embeddinggemma-300m-q8", 0),
                msg: "native load failed".into(),
            };
            *host.clip.lock().unwrap() = Slot::Building {
                attempt: attempt(2, "ViT-H-14-378-quickgelu__dfn5b", 0),
            };
        }
        let text_gen = host.text_gen.load(Ordering::SeqCst);
        let clip_gen = host.clip_gen.load(Ordering::SeqCst);

        host.retry_failed();

        assert!(matches!(&*host.text.lock().unwrap(), Slot::Idle));
        assert!(matches!(&*host.clip.lock().unwrap(), Slot::Building { .. }));
        assert_eq!(host.text_gen.load(Ordering::SeqCst), text_gen + 1);
        assert_eq!(host.clip_gen.load(Ordering::SeqCst), clip_gen);
    }

    /// The local DFN5B/EmbeddingGemma snapshots, if present, prove the FULL
    /// host path end to end: `apply` dispatches a background build, the slot
    /// transitions Building -> Ready, and `text()`/`clip()` then hand out a
    /// usable connector. Skips cleanly when the snapshots are absent (the
    /// gate machine has no weights). Lays out a temp `models_dir/<id>/`
    /// pointing at each snapshot so the host's `<id>/<file.path>` joins
    /// resolve exactly as they would post-download (L1 path layout).
    #[test]
    #[ignore = "needs the local EmbeddingGemma + DFN5B snapshots"]
    fn host_builds_real_sessions_and_reaches_ready() {
        let snaps = std::path::Path::new("/Users/bornman/spike-p7-embed/models");
        let gemma = snaps.join("embeddinggemma");
        let dfn = snaps.join("dfn5b");
        if !gemma.join("onnx/model_quantized.onnx").exists()
            || !dfn.join("visual/model.onnx").exists()
        {
            eprintln!(
                "skipping: embedder snapshots absent under {}",
                snaps.display()
            );
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let models_dir = tmp.path().join("models");
        // Build REAL id directories and symlink the snapshot's immediate
        // children in (not the id dir itself): the DFN5B visual tower's
        // external-data files load relative to `visual/model.onnx`, and
        // mirroring `visual/`+`textual/`+`onnx/` as their own symlinks keeps
        // that resolution identical to a direct snapshot path (the L3 test's
        // working layout) — a single directory-level symlink at the id made
        // ort's external-data resolution pathologically slow.
        let gemma_dir = models_dir.join("embeddinggemma-300m-q8");
        let dfn_dir = models_dir.join("ViT-H-14-378-quickgelu__dfn5b");
        std::fs::create_dir_all(&gemma_dir).unwrap();
        std::fs::create_dir_all(&dfn_dir).unwrap();
        std::os::unix::fs::symlink(gemma.join("onnx"), gemma_dir.join("onnx")).unwrap();
        std::os::unix::fs::symlink(
            gemma.join("tokenizer.json"),
            gemma_dir.join("tokenizer.json"),
        )
        .unwrap();
        std::os::unix::fs::symlink(dfn.join("visual"), dfn_dir.join("visual")).unwrap();
        std::os::unix::fs::symlink(dfn.join("textual"), dfn_dir.join("textual")).unwrap();

        let host = EmbedderHost::new();
        host.apply(
            &run_plan(
                Some("ViT-H-14-378-quickgelu__dfn5b"),
                Some("embeddinggemma-300m-q8"),
            ),
            EmbedderBackend::LocalOrt,
            TextEmbedderBackend::LocalOrt,
            &models_dir,
        );

        // The build is on a background thread (load is seconds — the DFN5B
        // visual tower especially: a 2.7 GB session over ~100 external-data
        // files; ~13 s for the pair under the debug profile in the spike).
        // Poll for readiness with a generous cap.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while (!host.text_ready() || !host.clip_ready()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let lines = host.debug_lines().join("\n");
        assert!(host.text_ready(), "text never reached Ready: {lines}");
        assert!(host.clip_ready(), "clip never reached Ready: {lines}");

        // A ready slot hands out a working connector. The trait methods
        // (`dimensions`/`embed_text`) need `Embedder` in scope.
        use photoproof_connectors::Embedder;
        let te = host.text().expect("text connector");
        assert_eq!(te.dimensions(), 768);
        let v = pollster::block_on(te.embed_text("a quiet harbor at dusk")).expect("embed");
        assert_eq!(v.vector.len(), 768);
        let ce = host.clip().expect("clip connector");
        assert_eq!(ce.dimensions(), 1024);

        host.shutdown();
        assert!(!host.text_ready(), "shutdown drops the sessions");
        assert!(!host.clip_ready());
    }
}
