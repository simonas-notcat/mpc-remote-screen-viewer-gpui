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
    use mpc_remote_screen_viewer_gpui::{mcp::TeacherServer, teacher};
    use rmcp::ServiceExt;
    use std::path::PathBuf;

    let mut socket = teacher::default_socket_path();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manual" => return Err("--manual was removed: use the force-teacher agent skill and local Markdown corpus; the device server needs no PDF".into()),
            "--socket" => {
                socket = PathBuf::from(
                    args.next()
                        .ok_or("--socket requires an absolute Unix socket path")?,
                )
            }
            "--help" | "-h" => {
                println!(
                    "Device MCP server (read-only by default)\n\nUsage: mpc-mcp [--socket PATH]\n\nNo manual or Poppler dependency. Use the force-teacher agent skill for local reference retrieval. For screen/highlight tools enable the robot button (Toggle MCP) in the viewer's top toolbar.\nSocket defaults to MPC_TEACHER_SOCKET or the viewer's private per-user temporary socket.\nThe adjacent Allow assistance switch globally enables device input: no permission timer or per-gesture approvals. Switch it off or toggle MCP off to cancel/release. No audio tools. Never retry uncertain mutations. Requested screens may be sent to your configured model."
                );
                return Ok(());
            }
            _ => return Err("Unknown option; use --help".into()),
        }
    }
    if !socket.is_absolute() {
        return Err("--socket must be absolute".into());
    }
    let server = TeacherServer::new(socket)
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
