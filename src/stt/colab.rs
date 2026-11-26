use std::{ffi::OsStr, io::ErrorKind, process::Stdio, time::Duration};

use async_trait::async_trait;
use tempfile::TempDir;
use tokio::{fs, process::Command, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::error::{AppError, Result};

use super::{SttInput, SttProvider, SttResult, openai::decode_verbose_json};

const WORKER: &str = include_str!("../../colab/transcribe.py");
const REMOTE_WORKER: &str = "/content/subflux-transcribe.py";
const REMOTE_AUDIO: &str = "/content/subflux-audio.flac";

pub(crate) struct ColabStt {
    verbose: bool,
    runtime: Mutex<Option<Runtime>>,
}

struct Runtime {
    workspace: TempDir,
    session: String,
    allocated: bool,
    worker_uploaded: bool,
}

impl Runtime {
    fn new() -> Result<Self> {
        let workspace = tempfile::Builder::new().prefix("subflux-").tempdir()?;
        let session = workspace
            .path()
            .file_name()
            .expect("temporary directory has a name")
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            workspace,
            session,
            allocated: false,
            worker_uploaded: false,
        })
    }

    async fn prepare(&mut self, verbose: bool, cancellation: &CancellationToken) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(AppError::Cancelled);
        }
        if !self.allocated {
            debug!(session = %self.session, "requesting Colab T4 runtime");
            if verbose {
                eprintln!("Requesting a Colab T4 runtime ({})", self.session);
            }
            self.command("new", ["--gpu", "T4"], verbose, &CancellationToken::new())
                .await?;
            self.allocated = true;
        }
        if !self.worker_uploaded {
            let worker = self.workspace.path().join("transcribe.py");
            fs::write(&worker, WORKER).await?;
            self.command(
                "upload",
                [worker.as_os_str(), REMOTE_WORKER.as_ref()],
                verbose,
                cancellation,
            )
            .await?;
            self.worker_uploaded = true;
        }
        Ok(())
    }

    async fn command<A: AsRef<OsStr>>(
        &self,
        operation: &str,
        args: impl IntoIterator<Item = A>,
        verbose: bool,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let mut command = Command::new("colab");
        command
            .args([operation, "-s", &self.session])
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if !verbose {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
        #[cfg(unix)]
        command.process_group(0);
        let status = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(AppError::Cancelled),
            status = command.status() => status.map_err(|error| {
                if error.kind() == ErrorKind::NotFound {
                    AppError::SttError("Google Colab CLI not found; install it with `uv tool install google-colab-cli`".into())
                } else {
                    AppError::SttError(format!("could not run `colab {operation}`: {error}"))
                }
            })?,
        };
        if status.success() {
            Ok(())
        } else {
            let hint = if operation == "new" {
                "; sign in with `colab usage` and check T4 availability"
            } else {
                ""
            };
            Err(AppError::SttError(format!(
                "`colab {operation}` failed ({status}){hint}"
            )))
        }
    }
}

impl ColabStt {
    pub(crate) const fn new(verbose: bool) -> Self {
        Self {
            verbose,
            runtime: Mutex::const_new(None),
        }
    }
}

#[async_trait]
impl SttProvider for ColabStt {
    async fn transcribe(
        &self,
        input: SttInput,
        cancellation: &CancellationToken,
    ) -> Result<SttResult> {
        let mut runtime = tokio::select! {
            guard = self.runtime.lock() => guard,
            () = cancellation.cancelled() => return Err(AppError::Cancelled),
        };
        if runtime.is_none() {
            *runtime = Some(Runtime::new()?);
        }
        let runtime = runtime
            .as_mut()
            .expect("Colab runtime state was initialized");
        runtime.prepare(self.verbose, cancellation).await?;

        let request = tempfile::Builder::new()
            .prefix("chunk-")
            .tempdir_in(runtime.workspace.path())?;
        let name = request
            .path()
            .file_name()
            .expect("chunk directory has a name");
        let remote_result = format!("/content/subflux-{}.json", name.to_string_lossy());
        let language = input.language.as_ref().map_or("", |value| {
            value.as_str().split('-').next().unwrap_or_default()
        });
        let execute = format!(
            "import runpy\nrunpy.run_path({REMOTE_WORKER:?})['run']({:?}, {remote_result:?})\n",
            language.to_ascii_lowercase()
        );
        let script = request.path().join("run.py");
        let result = request.path().join("result.json");
        fs::write(&script, execute).await?;
        runtime
            .command(
                "upload",
                [input.audio_path.as_os_str(), REMOTE_AUDIO.as_ref()],
                self.verbose,
                cancellation,
            )
            .await?;
        runtime
            .command(
                "exec",
                [
                    "--timeout".as_ref(),
                    "900".as_ref(),
                    "-f".as_ref(),
                    script.as_os_str(),
                ],
                self.verbose,
                cancellation,
            )
            .await?;
        runtime
            .command(
                "download",
                [remote_result.as_ref(), result.as_os_str()],
                self.verbose,
                cancellation,
            )
            .await?;
        decode_verbose_json(&fs::read(result).await?)
    }

    async fn close(&self) -> Result<()> {
        let mut guard = self.runtime.lock().await;
        let Some(runtime) = guard.as_ref().filter(|runtime| runtime.allocated) else {
            return Ok(());
        };
        let stopped = tokio::time::timeout(
            Duration::from_secs(60),
            runtime.command::<&str>("stop", [], self.verbose, &CancellationToken::new()),
        )
        .await;
        if matches!(stopped, Ok(Ok(()))) {
            *guard = None;
            return Ok(());
        }
        Err(AppError::SttError(format!(
            "could not stop the Colab runtime; run `colab stop -s {}`",
            runtime.session
        )))
    }
}
