//! Photoproof desktop shell. Contract: spec/UI.md, spec/CAPTURE.md §3–4.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // onnxruntime runtime-resolution FIRST, before any thread spawns: on a
    // `cuda-dynamic` (NVIDIA) build this finds the hardware-matched cuda13
    // libonnxruntime.so (sm_120) and exports ORT_DYLIB_PATH + LD_LIBRARY_PATH so
    // the isolated ort embedder helper dlopen's it when it builds its first session
    // (docs/PLAN-NVIDIA-LAUNCH.md). A no-op on the macOS/CPU builds — they keep
    // ort's bundled binary + CoreML/CPU. Must precede `set_var` for the WebKit
    // workaround and `run()` for the same single-threaded soundness reason.
    photoproof_desktop::ort_runtime::resolve();
    let internal_args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if photoproof_desktop::backup::is_offline_helper_smoke_arg(
        internal_args.first().map(std::ffi::OsString::as_os_str),
    ) {
        if internal_args.len() != 1 {
            eprintln!("offline backup/restore smoke helper accepts no command-line paths");
            std::process::exit(2);
        }
        std::process::exit(photoproof_desktop::backup::run_offline_helper_smoke());
    }
    if photoproof_desktop::backup::is_offline_helper_arg(
        internal_args.first().map(std::ffi::OsString::as_os_str),
    ) {
        if internal_args.len() != 1 {
            eprintln!("offline backup/restore helper accepts no command-line paths");
            std::process::exit(2);
        }
        std::process::exit(photoproof_desktop::backup::run_offline_helper());
    }
    if internal_args
        .first()
        .is_some_and(|arg| arg == "--photoproof-embedder-helper")
    {
        std::process::exit(run_embedder_helper(&internal_args[1..]));
    }
    if internal_args
        .iter()
        .any(|arg| arg == "--photoproof-capability-helper")
    {
        std::process::exit(photoproof_desktop::run_capability_probe_helper());
    }
    // Native-package acceptance lane: headless and isolated from Tauri/WebKit
    // so every CI host can exercise the actual packaged executable.
    let mut smoke_args = std::env::args_os().skip(1);
    if smoke_args.next().as_deref() == Some(std::ffi::OsStr::new("--installed-smoke")) {
        let Some(app_data) = smoke_args.next() else {
            eprintln!("--installed-smoke requires an isolated app-data directory");
            std::process::exit(2);
        };
        if smoke_args.next().is_some() {
            eprintln!("--installed-smoke accepts exactly one directory");
            std::process::exit(2);
        }
        match photoproof_desktop::installed_smoke::run(std::path::Path::new(&app_data)) {
            Ok(receipt) => {
                println!("{}", receipt.display());
                return;
            }
            Err(error) => {
                eprintln!("installed smoke failed: {error}");
                std::process::exit(1);
            }
        }
    }

    // WebKitGTK's DMABUF renderer crashes on NVIDIA + Wayland (Gdk protocol
    // error 71). Disable it there unless the user already decided; AMD/Intel
    // and X11 keep the fast path.
    // SAFETY: single-threaded — first statement of main, before GTK init.
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/proc/driver/nvidia").exists()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    photoproof_desktop::run();
}

const EMBEDDER_FRAME_MAGIC: [u8; 4] = *b"PPE1";
const EMBEDDER_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const EMBEDDER_MAX_TEXT_BYTES: usize = 1024 * 1024;
const EMBEDDER_MAX_IMAGE_BYTES: usize = EMBEDDER_MAX_FRAME_BYTES - 8;
const EMBEDDER_MAX_MODEL_ID_BYTES: usize = 512;
const EMBEDDER_MAX_VECTOR_DIMS: usize = 65_536;
const EMBEDDER_OP_TEXT: u8 = 1;
const EMBEDDER_OP_IMAGE: u8 = 2;
const EMBEDDER_OP_SHUTDOWN: u8 = 3;
const EMBEDDER_RESP_READY: u8 = 16;
const EMBEDDER_RESP_EMBEDDING: u8 = 17;
const EMBEDDER_RESP_ERROR: u8 = 18;
const EMBEDDER_RESP_EXECUTION: u8 = 19;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EmbedderReadyWire {
    model_id: String,
    dims: usize,
    sessions: Vec<EmbedderSessionWire>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EmbedderSessionWire {
    requested: Vec<String>,
    available: Vec<String>,
    registered: Vec<String>,
    selected: &'static str,
    actual: Vec<String>,
    fallback_reason: Option<String>,
    measurement: &'static str,
    profile_path: Option<String>,
}

fn run_embedder_helper(args: &[std::ffi::OsString]) -> i32 {
    use photoproof_connectors::Embedder;
    use photoproof_connectors::model_specs::{
        build_clip_embedder, build_text_embedder, is_known_clip_model, is_known_text_model,
    };
    use std::io::Write;

    if args.len() != 3 {
        eprintln!("embedder helper requires role, model id, and models directory");
        return 2;
    }
    let Some(role) = args[0].to_str() else {
        eprintln!("embedder helper role is not UTF-8");
        return 2;
    };
    let Some(model_id) = args[1].to_str() else {
        eprintln!("embedder helper model id is not UTF-8");
        return 2;
    };
    let models_dir = std::path::Path::new(&args[2]);
    let known = match role {
        "text" => is_known_text_model(model_id),
        "clip" => is_known_clip_model(model_id),
        _ => false,
    };
    if !known
        || model_id.is_empty()
        || model_id.len() > EMBEDDER_MAX_MODEL_ID_BYTES
        || !model_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        || !models_dir.is_absolute()
        || models_dir.as_os_str().len() > 32_768
    {
        eprintln!("embedder helper arguments failed validation");
        return 2;
    }

    let built = std::panic::catch_unwind(|| match role {
        "text" => build_text_embedder(model_id, models_dir),
        "clip" => build_clip_embedder(model_id, models_dir),
        _ => unreachable!("validated role"),
    });
    let embedder = match built {
        Ok(Ok(embedder)) => embedder,
        Ok(Err(error)) => {
            let _ = embedder_write_frame(
                std::io::stdout(),
                EMBEDDER_RESP_ERROR,
                format!("ort load failed: {error}").as_bytes(),
            );
            return 1;
        }
        Err(_) => {
            let _ =
                embedder_write_frame(std::io::stdout(), EMBEDDER_RESP_ERROR, b"ort load panicked");
            return 1;
        }
    };

    let ready = encode_embedder_execution(&embedder);
    let ready = match ready {
        Ok(ready) => ready,
        Err(error) => {
            eprintln!("serialize embedder ready payload: {error}");
            return 1;
        }
    };
    let mut stdout = std::io::stdout().lock();
    if embedder_write_frame(&mut stdout, EMBEDDER_RESP_READY, &ready).is_err()
        || stdout.flush().is_err()
    {
        return 1;
    }
    let mut stdin = std::io::stdin().lock();
    loop {
        let (opcode, body) = match embedder_read_frame(&mut stdin) {
            Ok(frame) => frame,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return 0,
            Err(error) => {
                eprintln!("embedder helper protocol read: {error}");
                return 1;
            }
        };
        if opcode == EMBEDDER_OP_SHUTDOWN {
            if !body.is_empty() {
                let _ = embedder_send_error(&mut stdout, "shutdown payload must be empty");
                continue;
            }
            return 0;
        }
        let result = match opcode {
            EMBEDDER_OP_TEXT if body.len() <= EMBEDDER_MAX_TEXT_BYTES => {
                match std::str::from_utf8(&body) {
                    Ok(text) => pollster::block_on(embedder.embed_text(text)),
                    Err(error) => Err(photoproof_connectors::ConnectorError::Decode(
                        error.to_string(),
                    )),
                }
            }
            EMBEDDER_OP_IMAGE => decode_embedder_image(&body)
                .and_then(|image| pollster::block_on(embedder.embed_image(&image))),
            EMBEDDER_OP_TEXT => Err(photoproof_connectors::ConnectorError::Backend {
                status: 413,
                message: "text payload exceeds protocol limit".into(),
            }),
            _ => Err(photoproof_connectors::ConnectorError::Backend {
                status: 400,
                message: format!("unsupported embedder helper opcode {opcode}"),
            }),
        };
        let execution = match encode_embedder_execution(&embedder) {
            Ok(execution) => execution,
            Err(error) => {
                let _ = embedder_send_error(&mut stdout, &error.to_string());
                continue;
            }
        };
        if embedder_write_frame(&mut stdout, EMBEDDER_RESP_EXECUTION, &execution).is_err() {
            return 1;
        }
        let wrote = match result {
            Ok(embedding) => encode_embedder_embedding(&embedding)
                .and_then(|body| embedder_write_frame(&mut stdout, EMBEDDER_RESP_EMBEDDING, &body)),
            Err(error) => embedder_send_error(&mut stdout, &error.to_string()),
        };
        if wrote.is_err() || stdout.flush().is_err() {
            return 1;
        }
    }
}

fn encode_embedder_execution(
    embedder: &photoproof_connectors::OrtEmbedder,
) -> std::io::Result<Vec<u8>> {
    use photoproof_connectors::Embedder;

    let execution = embedder.execution();
    let wire = EmbedderReadyWire {
        model_id: embedder.model_id().to_owned(),
        dims: embedder.dimensions(),
        sessions: execution
            .sessions
            .iter()
            .map(|session| EmbedderSessionWire {
                requested: session.requested.clone(),
                available: session.available.clone(),
                registered: session.registered.clone(),
                selected: match session.selected {
                    photoproof_connectors::ExecutionSelection::Cpu => "cpu",
                    photoproof_connectors::ExecutionSelection::CoreMl => "core-ml",
                    photoproof_connectors::ExecutionSelection::Cuda => "cuda",
                    photoproof_connectors::ExecutionSelection::TensorRt => "tensor-rt",
                    photoproof_connectors::ExecutionSelection::Unknown => "unknown",
                },
                actual: session.actual.clone(),
                fallback_reason: session.fallback_reason.clone(),
                measurement: session.measurement,
                profile_path: session.profile_path.clone(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&wire).map_err(std::io::Error::other)?;
    if bytes.len() > EMBEDDER_MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "embedder execution payload exceeds protocol limit",
        ));
    }
    Ok(bytes)
}

fn embedder_write_frame(
    mut writer: impl std::io::Write,
    opcode: u8,
    body: &[u8],
) -> std::io::Result<()> {
    if body.len() > EMBEDDER_MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "embedder helper frame exceeds limit",
        ));
    }
    writer.write_all(&EMBEDDER_FRAME_MAGIC)?;
    writer.write_all(&[opcode])?;
    writer.write_all(&(body.len() as u32).to_le_bytes())?;
    writer.write_all(body)
}

fn embedder_read_frame(mut reader: impl std::io::Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0_u8; 9];
    reader.read_exact(&mut header)?;
    if header[..4] != EMBEDDER_FRAME_MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "embedder helper protocol magic/version mismatch",
        ));
    }
    let len = u32::from_le_bytes(header[5..9].try_into().expect("frame length")) as usize;
    if len > EMBEDDER_MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "embedder helper frame exceeds limit",
        ));
    }
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body)?;
    Ok((header[4], body))
}

fn decode_embedder_image(
    body: &[u8],
) -> photoproof_connectors::ConnectorResult<photoproof_connectors::DecodedImage> {
    if body.len() < 8 {
        return Err(photoproof_connectors::ConnectorError::Decode(
            "short image payload".into(),
        ));
    }
    let width = u32::from_le_bytes(body[..4].try_into().expect("image width"));
    let height = u32::from_le_bytes(body[4..8].try_into().expect("image height"));
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| {
            photoproof_connectors::ConnectorError::Decode("image dimensions overflow".into())
        })?;
    if width == 0
        || height == 0
        || expected > EMBEDDER_MAX_IMAGE_BYTES
        || body.len() != expected + 8
    {
        return Err(photoproof_connectors::ConnectorError::Backend {
            status: 422,
            message: "invalid image payload".into(),
        });
    }
    Ok(photoproof_connectors::DecodedImage {
        rgb8: body[8..].to_vec(),
        width,
        height,
    })
}

fn encode_embedder_embedding(
    embedding: &photoproof_connectors::Embedding,
) -> std::io::Result<Vec<u8>> {
    if embedding.model_id.is_empty()
        || embedding.model_id.len() > EMBEDDER_MAX_MODEL_ID_BYTES
        || embedding.vector.is_empty()
        || embedding.vector.len() > EMBEDDER_MAX_VECTOR_DIMS
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid embedding response",
        ));
    }
    let mut body = Vec::with_capacity(6 + embedding.model_id.len() + embedding.vector.len() * 4);
    body.extend_from_slice(&(embedding.model_id.len() as u16).to_le_bytes());
    body.extend_from_slice(embedding.model_id.as_bytes());
    body.extend_from_slice(&(embedding.vector.len() as u32).to_le_bytes());
    for value in &embedding.vector {
        body.extend_from_slice(&value.to_le_bytes());
    }
    Ok(body)
}

fn embedder_send_error(stdout: &mut impl std::io::Write, message: &str) -> std::io::Result<()> {
    let message = if message.len() <= EMBEDDER_MAX_TEXT_BYTES {
        message.as_bytes()
    } else {
        b"embedder helper error exceeds protocol limit"
    };
    embedder_write_frame(stdout, EMBEDDER_RESP_ERROR, message)
}
