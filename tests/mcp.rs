//! Real stdio MCP smoke test. Synthetic pixels; no PDF, device connection or audio.
#![cfg(all(feature = "mcp", unix))]
use base64::{Engine, engine::general_purpose::STANDARD};
use mpc_remote_screen_viewer_gpui::{
    framebuffer::Framebuffer,
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

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: mpsc::Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}
impl McpProcess {
    fn start(socket: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mpc-mcp"))
            // Prove that initialization and all device tools need no Poppler/external tools.
            .env("PATH", "")
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
fn stdio_device_tools_work_without_manual_or_external_tools() {
    let dir = TestDirectory::new();
    let socket = dir.0.join("viewer.sock");
    let mut client = McpProcess::start(&socket);
    let init = client.rpc("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "mpc-mcp", "{init}");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("read-only by default")
    );
    client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let tools = client.rpc("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 11);
    let state = client.tool("mpc_get_state", json!({}));
    assert_eq!(state["structuredContent"]["viewer_available"], false);
    assert!(state["structuredContent"].get("manual").is_none());
    assert!(
        state["structuredContent"]
            .get("manual_compatibility")
            .is_none()
    );
    for name in ["mpc_search_manual", "mpc_read_manual"] {
        let removed = client.rpc("tools/call", json!({"name": name, "arguments": {}}));
        assert!(removed.get("error").is_some() || removed["result"]["isError"] == true);
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
    // Tools are available, but enabling screen sharing never authorizes input.
    for (name, args) in [
        ("mpc_click", json!({"target": {"x": 1, "y": 1}})),
        ("mpc_double_tap", json!({"target": {"x": 1, "y": 1}})),
        (
            "mpc_drag",
            json!({"start": {"x": 1, "y": 1}, "end": {"x": 2, "y": 2}}),
        ),
        (
            "mpc_scroll",
            json!({"target": {"x": 1, "y": 1}, "amount": 1}),
        ),
        ("mpc_press_key", json!({"key": "enter"})),
        ("mpc_type_text", json!({"text": "Synthetic"})),
    ] {
        let mut args = args;
        args["observation_id"] = json!(observation);
        let result = client.tool(name, args);
        assert_eq!(result["isError"], true, "{name}: {result}");
        assert!(result.to_string().contains("Read-only"), "{name}: {result}");
    }
    assert_eq!(client.tool("mpc_cancel", json!({}))["isError"], true);
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
    let removed = Command::new(env!("CARGO_BIN_EXE_mpc-mcp"))
        .args(["--manual", "nonexistent.pdf"])
        .output()
        .unwrap();
    assert!(!removed.status.success());
    assert!(removed.stdout.is_empty());
    assert!(
        String::from_utf8(removed.stderr)
            .unwrap()
            .contains("agent skill")
    );
    let invalid = Command::new(env!("CARGO_BIN_EXE_mpc-mcp"))
        .arg("--unknown")
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());
}
