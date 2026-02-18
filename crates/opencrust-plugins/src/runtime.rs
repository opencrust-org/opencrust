use crate::manifest::PluginManifest;
use crate::traits::{Capability, Plugin, PluginInput, PluginOutput};
use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::path::PathBuf;
use wasmtime::{Config, Engine, Linker, Module, Store};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi::preview1::{self, WasiP1Ctx};

pub struct WasmRuntime {
    manifest: PluginManifest,
    engine: Engine,
    module: Module,
}

struct WasmState {
    ctx: WasiP1Ctx,
}

impl WasmRuntime {
    pub fn new(manifest: PluginManifest, wasm_path: PathBuf) -> Result<Self> {
        let mut config = Config::new();
        config.async_support(true);
        config.epoch_interruption(true);

        let engine =
            Engine::new(&config).map_err(|e| Error::Plugin(format!("engine error: {e}")))?;

        let wasm_bytes = std::fs::read(&wasm_path).map_err(|e| {
            Error::Plugin(format!("failed to read wasm {}: {e}", wasm_path.display()))
        })?;
        let module = Module::new(&engine, &wasm_bytes)
            .map_err(|e| Error::Plugin(format!("module error: {e}")))?;

        Ok(Self {
            manifest,
            engine,
            module,
        })
    }
}

#[async_trait]
impl Plugin for WasmRuntime {
    fn name(&self) -> &str {
        &self.manifest.plugin.name
    }

    fn description(&self) -> &str {
        &self.manifest.plugin.description
    }

    fn capabilities(&self) -> Vec<Capability> {
        let mut caps = Vec::new();
        caps.push(Capability::Filesystem(self.manifest.permissions.filesystem));
        if !self.manifest.permissions.network.is_empty() {
            caps.push(Capability::Network(
                self.manifest.permissions.network.clone(),
            ));
        }
        if !self.manifest.permissions.env_vars.is_empty() {
            caps.push(Capability::EnvVars(
                self.manifest.permissions.env_vars.clone(),
            ));
        }
        caps
    }

    async fn execute(&self, input: PluginInput) -> Result<PluginOutput> {
        let mut linker = Linker::new(&self.engine);
        preview1::add_to_linker_async(&mut linker, |s: &mut WasmState| &mut s.ctx)
            .map_err(|e| Error::Plugin(format!("linker error: {e}")))?;

        let mut builder = WasiCtxBuilder::new();
        builder.args(&input.args);

        for (k, v) in &input.env {
            if self.manifest.permissions.env_vars.contains(k) {
                builder.env(k, v);
            }
        }

        // Output capture via pipes
        let stdout = wasmtime_wasi::pipe::MemoryOutputPipe::new(4096);
        let stderr = wasmtime_wasi::pipe::MemoryOutputPipe::new(4096);
        builder.stdout(stdout.clone());
        builder.stderr(stderr.clone());

        // Input
        if !input.stdin.is_empty() {
            let stdin = wasmtime_wasi::pipe::MemoryInputPipe::new(input.stdin.clone());
            builder.stdin(stdin);
        }

        let ctx = builder.build_p1();

        let state = WasmState { ctx };
        let mut store = Store::new(&self.engine, state);

        // Timeout
        store.set_epoch_deadline(1);
        let engine = self.engine.clone();
        let timeout_secs = self.manifest.limits.timeout_secs;

        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(timeout_secs)).await;
            engine.increment_epoch();
        });

        let instance = linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|e| Error::Plugin(format!("instantiation error: {e}")))?;

        let func = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| Error::Plugin(format!("missing _start: {e}")))?;

        let res = func.call_async(&mut store, ()).await;

        let stdout_data = stdout.contents().into();
        let stderr_data = stderr.contents().into();

        let status = match res {
            Ok(_) => 0,
            Err(e) => {
                if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    exit.0
                } else if e.root_cause().to_string().contains("interrupted") {
                    return Err(Error::Plugin("execution timed out".into()));
                } else {
                    return Err(Error::Plugin(format!("execution error: {e}")));
                }
            }
        };

        Ok(PluginOutput {
            stdout: stdout_data,
            stderr: stderr_data,
            status,
        })
    }
}
