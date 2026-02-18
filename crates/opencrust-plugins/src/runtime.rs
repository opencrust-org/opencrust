use crate::manifest::PluginManifest;
use crate::traits::{Capability, Plugin, PluginInput, PluginOutput};
use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::{DirPerms, FilePerms, SocketAddrUse, WasiCtxBuilder};

pub struct WasmRuntime {
    manifest: PluginManifest,
    engine: Engine,
    module: Module,
    plugin_root: PathBuf,
    ticker_handle: tokio::task::JoinHandle<()>,
}

struct WasmState {
    ctx: WasiP1Ctx,
    limits: StoreLimits,
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.ticker_handle.abort();
    }
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
        let plugin_root = wasm_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let ticker_engine = engine.clone();
        let ticker_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                ticker_engine.increment_epoch();
            }
        });

        Ok(Self {
            manifest,
            engine,
            module,
            plugin_root,
            ticker_handle,
        })
    }

    fn configure_filesystem(&self, builder: &mut WasiCtxBuilder) -> Result<()> {
        let read_paths = &self.manifest.permissions.filesystem_read_paths;
        let write_paths = &self.manifest.permissions.filesystem_write_paths;

        if !self.manifest.permissions.filesystem {
            if !read_paths.is_empty() || !write_paths.is_empty() {
                return Err(Error::Plugin(
                    "filesystem paths were provided but filesystem=false in plugin permissions"
                        .to_string(),
                ));
            }
            return Ok(());
        }

        // Filesystem enabled: always scope access to explicit preopened dirs.
        // If none are configured, default to plugin root as read-only.
        let effective_read_paths = if read_paths.is_empty() && write_paths.is_empty() {
            vec![self.plugin_root.display().to_string()]
        } else {
            read_paths.clone()
        };

        let mut mounts: BTreeMap<PathBuf, bool> = BTreeMap::new();
        for raw in effective_read_paths {
            let host_path = normalize_scoped_path(&self.plugin_root, &raw, false)?;
            mounts.entry(host_path).or_insert(false);
        }
        for raw in write_paths {
            let host_path = normalize_scoped_path(&self.plugin_root, raw, true)?;
            mounts.insert(host_path, true);
        }

        for (idx, (host_path, writable)) in mounts.into_iter().enumerate() {
            let guest_path = format!("mnt{idx}");
            let dir_perms = if writable {
                DirPerms::READ | DirPerms::MUTATE
            } else {
                DirPerms::READ
            };
            let file_perms = if writable {
                FilePerms::READ | FilePerms::WRITE
            } else {
                FilePerms::READ
            };

            builder
                .preopened_dir(&host_path, &guest_path, dir_perms, file_perms)
                .map_err(|e| {
                    Error::Plugin(format!(
                        "failed to preopen filesystem path {}: {e}",
                        host_path.display()
                    ))
                })?;
        }

        Ok(())
    }

    fn configure_network(&self, builder: &mut WasiCtxBuilder) -> Result<()> {
        if self.manifest.permissions.network.is_empty() {
            return Ok(());
        }

        let allowed_ips = Arc::new(resolve_allowlisted_ips(&self.manifest.permissions.network)?);
        builder.allow_ip_name_lookup(true);
        builder.allow_tcp(true);
        builder.allow_udp(true);
        builder.socket_addr_check(move |addr, reason| {
            let allowed_ips = Arc::clone(&allowed_ips);
            Box::pin(async move {
                match reason {
                    SocketAddrUse::TcpConnect
                    | SocketAddrUse::UdpConnect
                    | SocketAddrUse::UdpOutgoingDatagram => allowed_ips.contains(&addr.ip()),
                    SocketAddrUse::TcpBind | SocketAddrUse::UdpBind => false,
                }
            })
        });

        Ok(())
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
        if self.manifest.permissions.filesystem {
            caps.push(Capability::Filesystem {
                read_paths: self.manifest.permissions.filesystem_read_paths.clone(),
                write_paths: self.manifest.permissions.filesystem_write_paths.clone(),
            });
        }
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
        self.configure_filesystem(&mut builder)?;
        self.configure_network(&mut builder)?;

        for (k, v) in &input.env {
            if self.manifest.permissions.env_vars.contains(k) {
                builder.env(k, v);
            }
        }

        // Output capture via bounded pipes.
        let max_output_bytes = self.manifest.limits.max_output_bytes.max(1);
        let stdout = wasmtime_wasi::pipe::MemoryOutputPipe::new(max_output_bytes);
        let stderr = wasmtime_wasi::pipe::MemoryOutputPipe::new(max_output_bytes);
        builder.stdout(stdout.clone());
        builder.stderr(stderr.clone());

        // Input
        if !input.stdin.is_empty() {
            let stdin = wasmtime_wasi::pipe::MemoryInputPipe::new(input.stdin.clone());
            builder.stdin(stdin);
        }

        let ctx = builder.build_p1();
        let max_memory_bytes = self
            .manifest
            .limits
            .max_memory_mb
            .saturating_mul(1024 * 1024)
            .min(usize::MAX as u64) as usize;
        let limits = StoreLimitsBuilder::new()
            .memory_size(max_memory_bytes)
            .build();

        let state = WasmState { ctx, limits };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);

        // Timeout
        // We set the deadline to the current engine epoch + timeout_secs.
        // The background ticker increments the epoch every second.
        let timeout_secs = self.manifest.limits.timeout_secs.max(1);
        store.set_epoch_deadline(timeout_secs);

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
                let root = e.root_cause().to_string();
                if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    exit.0
                } else if root.contains("interrupted") {
                    return Err(Error::Plugin("execution timed out".into()));
                } else if root.contains("write beyond capacity of MemoryOutputPipe") {
                    return Err(Error::Plugin(format!(
                        "plugin output exceeded limit ({} bytes per stream)",
                        max_output_bytes
                    )));
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

fn normalize_scoped_path(
    plugin_root: &Path,
    raw: &str,
    create_if_missing: bool,
) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(Error::Plugin(
            "filesystem path entries cannot be empty".to_string(),
        ));
    }

    let mut path = PathBuf::from(raw);
    if path.is_relative() {
        path = plugin_root.join(path);
    }
    if create_if_missing {
        std::fs::create_dir_all(&path).map_err(|e| {
            Error::Plugin(format!(
                "failed to create writable filesystem path {}: {e}",
                path.display()
            ))
        })?;
    }
    if !path.exists() {
        return Err(Error::Plugin(format!(
            "filesystem path does not exist: {}",
            path.display()
        )));
    }
    path.canonicalize().map_err(|e| {
        Error::Plugin(format!(
            "failed to canonicalize path {}: {e}",
            path.display()
        ))
    })
}

fn resolve_allowlisted_ips(domains: &[String]) -> Result<HashSet<IpAddr>> {
    let mut ips = HashSet::new();
    for domain in domains {
        let domain = domain.trim();
        if domain.is_empty() {
            continue;
        }

        let query = format!("{domain}:0");
        let resolved = query.to_socket_addrs().map_err(|e| {
            Error::Plugin(format!(
                "failed to resolve allowlisted domain '{domain}': {e}"
            ))
        })?;

        let mut resolved_any = false;
        for addr in resolved {
            ips.insert(addr.ip());
            resolved_any = true;
        }
        if !resolved_any {
            return Err(Error::Plugin(format!(
                "allowlisted domain '{domain}' resolved to no addresses"
            )));
        }
    }

    if ips.is_empty() {
        return Err(Error::Plugin(
            "network permission enabled but no allowlisted domains were resolved".to_string(),
        ));
    }

    Ok(ips)
}

#[cfg(test)]
mod tests {
    use super::{normalize_scoped_path, resolve_allowlisted_ips};
    use std::path::Path;

    #[test]
    fn resolve_allowlisted_ips_handles_localhost() {
        let ips = resolve_allowlisted_ips(&["localhost".to_string()]).unwrap();
        assert!(!ips.is_empty());
    }

    #[test]
    fn normalize_scoped_path_creates_writable_path() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("opencrust-plugin-test-{nanos}"));
        std::fs::create_dir_all(&root).unwrap();
        let scoped = normalize_scoped_path(Path::new(&root), "rw-data", true).unwrap();
        assert!(scoped.exists());
    }
}
