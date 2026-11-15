//! Stdio is reserved exclusively for MCP protocol messages; diagnostics go to stderr.
#[cfg(unix)]
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("mpc-mcp: {error}");
        std::process::exit(1);
    }
}
#[cfg(unix)]
async fn run() -> Result<(), String> {
    use mpc_remote_screen_viewer_gpui::{manual::Manual, mcp::TeacherServer, teacher};
    use rmcp::ServiceExt;
    use std::path::PathBuf;

    let mut manual = None;
    let mut socket = teacher::default_socket_path();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manual" => {
                manual = Some(PathBuf::from(
                    args.next().ok_or("--manual requires a PDF path")?,
                ))
            }
            "--socket" => {
                socket = PathBuf::from(
                    args.next()
                        .ok_or("--socket requires an absolute Unix socket path")?,
                )
            }
            "--help" | "-h" => {
                println!(
                    "Force teacher MCP server (read-only by default)\n\nUsage: mpc-mcp --manual '/absolute/path/Force - User Guide - v3.10.1.pdf' [--socket PATH]\n\nRequires Poppler pdftotext (pdftoppm for page images).\nManual tools work offline. For screen/highlight tools enable View → AI Teacher (read only) in the running viewer.\nSocket defaults to MPC_TEACHER_SOCKET or the viewer's private per-user temporary socket.\nAssisted input proposals require a local 60-second lease and approval of EVERY gesture in the viewer. No audio tools. Never retry uncertain mutations. Screens/manual excerpts may be sent to your configured model."
                );
                return Ok(());
            }
            _ => return Err("Unknown option; use --help".into()),
        }
    }
    if !socket.is_absolute() {
        return Err("--socket must be absolute".into());
    }
    let path = manual.ok_or("--manual is required; use --help")?;
    let manual = Manual::load_force(&path)?;
    let server = TeacherServer::new(manual, socket)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| e.to_string())?;
    server.waiting().await.map_err(|e| e.to_string())?;
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    eprintln!("mpc-mcp currently supports Unix (Linux/macOS) only");
    std::process::exit(1);
}
