//! Real stdio MCP smoke test. Synthetic PDF/pixels; no device connection or audio.
#![cfg(all(feature = "mcp", unix))]
use base64::{Engine, engine::general_purpose::STANDARD};
use mpc_remote_screen_viewer_gpui::{
    framebuffer::Framebuffer,
    manual::DOCUMENT_ID,
    protocol::{Bounds, DeviceInfo, FrameUpdate, PixelFormat, Rectangle, ServerInit},
    session::{Snapshot, State},
    teacher::{Bridge, Rect},
};
use serde_json::{Value, json};
use std::{
    fs::{self, DirBuilder},
    io::{BufRead, BufReader, Write},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, RwLock, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("mpc-wire-{suffix}"));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Minimal genuine PDF, containing only synthetic reference text.
fn write_pdf(path: &Path) {
    let pages = [
        "Force User Guide\nManual Version v3.10.1",
        "Sample Edit\nUse Chop Mode to slice a drum loop.\n2",
        "Mixer\nAdjust volume and pan.\n3",
    ];
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [4 0 R 6 0 R 8 0 R] /Count 3 >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    for (index, text) in pages.into_iter().enumerate() {
        let stream = format!(
            "BT /F1 12 Tf 20 370 Td 18 TL {} ET",
            text.lines()
                .map(|line| format!("({line}) Tj T*"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + index * 2));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fs::write(path, pdf).unwrap();
}
struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: mpsc::Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}
impl McpProcess {
    fn start(manual: &Path, socket: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mpc-mcp"))
            .arg("--manual")
            .arg(manual)
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, responses) = mpsc::sync_channel(8);
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let result = line.map_err(|e| e.to_string()).and_then(|line| {
                    serde_json::from_str(&line)
                        .map_err(|e| format!("Non-protocol stdout: {e}: {line}"))
                });
                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            responses,
            reader: Some(reader),
            next_id: 0,
        }
    }
    fn send(&mut self, value: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{value}").unwrap();
        stdin.flush().unwrap();
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(15))
                .expect("MCP response deadline")
                .unwrap();
            if response["id"] == id {
                return response;
            }
        }
    }
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
}
impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn synthetic_source() -> Arc<Mutex<Snapshot>> {
    let init = ServerInit {
        width: 320,
        height: 200,
        rotation: 1,
        pixel_format: PixelFormat([32, 16, 8, 8, 8, 0, 8]),
        name: "Synthetic Force".into(),
        input_capabilities: 7,
        audio_available: true,
    };
    let mut frame = Framebuffer::new(init.clone()).unwrap();
    frame.apply(&FrameUpdate {
        id: 1,
        quality: 0,
        rectangles: vec![Rectangle {
            bounds: Bounds {
                x: 0,
                y: 0,
                width: 320,
                height: 200,
            },
            codec: 3,
            drop_bits: 0,
            encoded: &[0, 0, 255, 0],
        }],
    });
    Arc::new(Mutex::new(Snapshot {
        state: State::Streaming,
        screen: Some(init),
        device: Some(DeviceInfo {
            model: "Akai Force".into(),
            firmware: "3.10.1".into(),
            serial: "not-exported".into(),
        }),
        framebuffer: Some(Arc::new(RwLock::new(frame))),
        ..Snapshot::default()
    }))
}
#[test]
fn stdio_initialize_manual_images_screen_and_local_highlight_when_poppler_available() {
    if !Command::new("pdftotext")
        .arg("-v")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!(
            "Skipping external Poppler smoke test; install Poppler. The MCP CI job requires it."
        );
        return;
    }
    let dir = TestDirectory::new();
    let pdf = dir.0.join("synthetic.pdf");
    write_pdf(&pdf);
    let socket = dir.0.join("viewer.sock");
    let mut client = McpProcess::start(&pdf, &socket);
    let init = client.rpc("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "mpc-mcp", "{init}");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("read-only teacher")
    );
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = client.rpc("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 6);
    let state = client.tool("mpc_get_state", json!({}));
    assert_eq!(state["structuredContent"]["viewer_available"], false);
    assert_eq!(state["structuredContent"]["manual"]["pdf_pages"], 3);
    let search = client.tool("mpc_search_manual", json!({"query": "chop drum loop"}));
    assert_eq!(search["structuredContent"]["results"][0]["pdf_page"], 2);
    let read = client.tool(
        "mpc_read_manual",
        json!({"document_id": DOCUMENT_ID, "start_page": 2}),
    );
    assert!(
        read["structuredContent"]["pages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Chop Mode")
    );
    assert_eq!(
        client.tool(
            "mpc_read_manual",
            json!({"document_id": DOCUMENT_ID, "start_page": 0})
        )["isError"],
        true
    );
    if Command::new("pdftoppm")
        .arg("-v")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        let read = client.tool(
            "mpc_read_manual",
            json!({"document_id": DOCUMENT_ID, "start_page": 2, "include_image": true}),
        );
        let image = read["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "image")
            .unwrap();
        let bytes = STANDARD.decode(image["data"].as_str().unwrap()).unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
    }
    let bridge = Bridge::start(socket).unwrap();
    bridge.teacher().set_source(Some(synthetic_source()));
    let state = client.tool("mpc_get_state", json!({}));
    assert_eq!(state["structuredContent"]["device"]["model"], "Akai Force");
    assert!(!state.to_string().contains("not-exported"));
    let screen = client.tool("mpc_get_screen", json!({"max_size": 128}));
    let data = &screen["structuredContent"];
    let observation = data["observation_id"].as_str().unwrap();
    assert_eq!(
        (data["image_width"].as_u64(), data["image_height"].as_u64()),
        (Some(80), Some(128))
    );
    let image = screen["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "image")
        .unwrap();
    assert!(
        image::load_from_memory(&STANDARD.decode(image["data"].as_str().unwrap()).unwrap()).is_ok()
    );
    let highlight = client.tool(
        "mpc_highlight",
        json!({"observation_id": observation,
        "bounds": {"x": 10, "y": 20, "width": 20, "height": 30}, "label": "Next step"}),
    );
    assert_eq!(highlight["structuredContent"]["device_input_sent"], false);
    assert_eq!(
        bridge.teacher().overlay().unwrap().bounds,
        Rect {
            x: 25,
            y: 50,
            width: 50,
            height: 75
        }
    );
    client.tool("mpc_clear_highlight", json!({}));
    assert!(bridge.teacher().overlay().is_none());
    bridge.teacher().set_source(None);
    assert_eq!(
        client.tool(
            "mpc_highlight",
            json!({"observation_id": observation,
        "bounds": {"x": 10, "y": 20, "width": 20, "height": 30}})
        )["isError"],
        true
    );
    let mutation = client.rpc(
        "tools/call",
        json!({"name": "mpc_click", "arguments": {"x": 1, "y": 1}}),
    );
    assert!(mutation.get("error").is_some() || mutation["result"]["isError"] == true);
}

/// Explicit local validation only; the copyrighted user manual is not a fixture.
#[test]
#[ignore = "set MPC_FORCE_MANUAL to the approved Force v3.10.1 PDF"]
fn supplied_force_manual_import_search_and_page_image() {
    let manual = std::env::var_os("MPC_FORCE_MANUAL")
        .expect("Set MPC_FORCE_MANUAL to the approved PDF path");
    let dir = TestDirectory::new();
    let mut client = McpProcess::start(Path::new(&manual), &dir.0.join("absent-viewer.sock"));
    let init = client.rpc("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "manual-check", "version": "1"}}));
    assert!(init.get("error").is_none(), "{init}");
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let state = client.tool("mpc_get_state", json!({}));
    let meta = &state["structuredContent"]["manual"];
    assert_eq!(meta["pdf_pages"], 468);
    assert_eq!(meta["manual_version"], "v3.10.1");
    assert_eq!(meta["sha256"].as_str().unwrap().len(), 64);
    for query in ["Chop Mode", "Track Mixer", "Remote Screen"] {
        let result = client.tool("mpc_search_manual", json!({"query": query, "limit": 3}));
        let hits = result["structuredContent"]["results"].as_array().unwrap();
        assert!(!hits.is_empty());
        eprintln!(
            "{query}: {}",
            hits.iter()
                .map(|h| h["citation"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let page = client.tool(
        "mpc_read_manual",
        json!({"document_id": DOCUMENT_ID, "start_page": 248, "include_image": true}),
    );
    assert_ne!(page["isError"], true, "{page}");
    let image = page["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "image")
        .unwrap();
    let decoded =
        image::load_from_memory(&STANDARD.decode(image["data"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(decoded.width().max(decoded.height()), 1400);
}

#[test]
fn help_needs_neither_manual_nor_viewer_and_invalid_cli_is_stderr_only() {
    let help = Command::new(env!("CARGO_BIN_EXE_mpc-mcp"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("read-only")
    );
    let invalid = Command::new(env!("CARGO_BIN_EXE_mpc-mcp"))
        .arg("--unknown")
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());
}
