use crate::proto::{exec_stream_input, exec_stream_output, ExecStreamInput, ExecStreamOutput};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tonic::Status;
use tracing::{debug, error, info};

const CHUNK_SIZE: usize = 4096;

#[allow(clippy::too_many_lines)]
pub async fn run_stream(
    mut input_stream: tonic::Streaming<ExecStreamInput>,
    tx: mpsc::Sender<Result<ExecStreamOutput, Status>>,
) {
    // First message must be a start command
    let start_req = match input_stream.message().await {
        Ok(Some(msg)) => {
            if let Some(exec_stream_input::Input::Start(req)) = msg.input {
                req
            } else {
                let _ = tx
                    .send(Err(Status::invalid_argument(
                        "First message must be a start command",
                    )))
                    .await;
                return;
            }
        }
        Ok(None) => return,
        Err(e) => {
            let _ = tx.send(Err(e)).await;
            return;
        }
    };

    debug!("Stream exec: {} {:?}", start_req.command, start_req.args);

    // Spawn process
    let mut cmd = Command::new(&start_req.command);
    cmd.args(&start_req.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    if !start_req.working_dir.is_empty() {
        cmd.current_dir(&start_req.working_dir);
    }

    for (key, value) in &start_req.env {
        cmd.env(key, value);
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = tx
                .send(Err(Status::internal(format!(
                    "Failed to spawn process: {e}"
                ))))
                .await;
            return;
        }
    };

    let mut child_stdin = child.stdin.take();
    let mut child_stdout = child.stdout.take().unwrap();
    let mut child_stderr = child.stderr.take().unwrap();

    // Task: forward client stdin to process stdin
    let stdin_handle = tokio::spawn(async move {
        while let Ok(Some(msg)) = input_stream.message().await {
            match msg.input {
                Some(exec_stream_input::Input::StdinData(data)) => {
                    if let Some(ref mut stdin) = child_stdin {
                        if stdin.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                }
                Some(exec_stream_input::Input::CloseStdin(true)) => {
                    // Drop stdin to signal EOF
                    child_stdin.take();
                    break;
                }
                _ => break,
            }
        }
        // Drop stdin on stream end
        drop(child_stdin);
    });

    // Task: read stdout and send to client
    let tx_stdout = tx.clone();
    let stdout_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            match child_stdout.read(&mut buf).await {
                Ok(n) if n > 0 => {
                    let msg = ExecStreamOutput {
                        output: Some(exec_stream_output::Output::StdoutData(buf[..n].to_vec())),
                    };
                    if tx_stdout.send(Ok(msg)).await.is_err() {
                        break;
                    }
                }
                Ok(_) | Err(_) => break,
            }
        }
    });

    // Task: read stderr and send to client
    let tx_stderr = tx.clone();
    let stderr_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            match child_stderr.read(&mut buf).await {
                Ok(n) if n > 0 => {
                    let msg = ExecStreamOutput {
                        output: Some(exec_stream_output::Output::StderrData(buf[..n].to_vec())),
                    };
                    if tx_stderr.send(Ok(msg)).await.is_err() {
                        break;
                    }
                }
                Ok(_) | Err(_) => break,
            }
        }
    });

    // Wait for stdout/stderr to finish
    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    // Wait for process to exit
    let exit_code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            error!("Failed to wait for process: {}", e);
            -1
        }
    };

    // Cancel stdin forwarder
    stdin_handle.abort();

    info!("Stream exec finished with exit code {}", exit_code);

    // Send exit code as final message
    let _ = tx
        .send(Ok(ExecStreamOutput {
            output: Some(exec_stream_output::Output::ExitCode(exit_code)),
        }))
        .await;
}
