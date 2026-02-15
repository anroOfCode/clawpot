use crate::proto::{ExecRequest, ExecResponse};
use anyhow::Result;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, warn};

const EXEC_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run_command(req: ExecRequest) -> Result<ExecResponse> {
    debug!("Executing: {} {:?}", req.command, req.args);

    let mut cmd = Command::new(&req.command);
    cmd.args(&req.args);

    if !req.working_dir.is_empty() {
        cmd.current_dir(&req.working_dir);
    }

    for (key, value) in &req.env {
        cmd.env(key, value);
    }

    let output = tokio::time::timeout(EXEC_TIMEOUT, cmd.output()).await;

    match output {
        Ok(Ok(output)) => Ok(ExecResponse {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        }),
        Ok(Err(e)) => {
            warn!("Command execution failed: {}", e);
            Ok(ExecResponse {
                exit_code: -1,
                stdout: Vec::new(),
                stderr: format!("Failed to execute command: {e}\n").into_bytes(),
            })
        }
        Err(_) => {
            warn!("Command timed out after {:?}", EXEC_TIMEOUT);
            Ok(ExecResponse {
                exit_code: -1,
                stdout: Vec::new(),
                stderr: format!(
                    "Command timed out after {} seconds\n",
                    EXEC_TIMEOUT.as_secs()
                )
                .into_bytes(),
            })
        }
    }
}
