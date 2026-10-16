use crate::manifest::PluginManifest;
use crate::traits::{Capability, Plugin, PluginInput, PluginOutput};
use async_trait::async_trait;
use opencrust_common::{Error, Result};
use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::sockets::SocketAddrUse;
use wasmtime_wasi::{FsPerms, WasiCtxBuilder};

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
            // Start one period from now: `interval` would otherwise tick
            // immediately and cut the first timeout second short.
            let period = tokio::time::Duration::from_secs(1);
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
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
            let perms = if writable {
                FsPerms::ReadWrite
            } else {
                FsPerms::ReadOnly
            };

            builder
                .preopened_dir(&host_path, &guest_path, perms)
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
            Box::pin(async move { socket_addr_allowed(&allowed_ips, addr, reason) })
        });

        Ok(())
    }
}

/// Outbound-only network policy for plugins with an allowlist.
///
/// Remote endpoints must be allowlisted. Binds are permitted only for the
/// implicit ephemeral bind (unspecified address, port 0) that wasmtime-wasi
/// checks before an unbound socket connects or sends. Listening and accepting
/// are always denied.
fn socket_addr_allowed(
    allowed_ips: &HashSet<IpAddr>,
    addr: SocketAddr,
    reason: SocketAddrUse,
) -> bool {
    match reason {
        SocketAddrUse::TcpConnect | SocketAddrUse::UdpSend | SocketAddrUse::UdpReceive => {
            allowed_ips.contains(&addr.ip())
        }
        SocketAddrUse::TcpBind | SocketAddrUse::UdpBind => {
            addr.ip().is_unspecified() && addr.port() == 0
        }
        SocketAddrUse::TcpListen | SocketAddrUse::TcpAccept => false,
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
        p1::add_to_linker_async(&mut linker, |s: &mut WasmState| &mut s.ctx)
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
        let stdout = MemoryOutputPipe::new(max_output_bytes);
        let stderr = MemoryOutputPipe::new(max_output_bytes);
        builder.stdout(stdout.clone());
        builder.stderr(stderr.clone());

        // Input
        if !input.stdin.is_empty() {
            let stdin = MemoryInputPipe::new(input.stdin.clone());
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
                } else if e.downcast_ref::<Trap>() == Some(&Trap::Interrupt) {
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

    let path = PathBuf::from(raw);

    // Reject absolute paths — plugins must stay within their root.
    if path.is_absolute() {
        return Err(Error::Plugin(format!(
            "absolute filesystem paths are not allowed in plugin permissions: {}",
            path.display()
        )));
    }

    let path = plugin_root.join(path);

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
    let canonical = path.canonicalize().map_err(|e| {
        Error::Plugin(format!(
            "failed to canonicalize path {}: {e}",
            path.display()
        ))
    })?;

    // Boundary check: the canonical path must still be inside plugin_root.
    let canonical_root = plugin_root.canonicalize().map_err(|e| {
        Error::Plugin(format!(
            "failed to canonicalize plugin root {}: {e}",
            plugin_root.display()
        ))
    })?;

    if !canonical.starts_with(&canonical_root) {
        return Err(Error::Plugin(format!(
            "path escapes plugin root: {} is outside {}",
            canonical.display(),
            canonical_root.display()
        )));
    }

    Ok(canonical)
}

/// Returns `true` if the IP address is private, loopback, or link-local.
fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()                          // 127.0.0.0/8
                || v4.octets()[0] == 10                // 10.0.0.0/8
                || (v4.octets()[0] == 172 && (16..=31).contains(&v4.octets()[1])) // 172.16.0.0/12
                || (v4.octets()[0] == 192 && v4.octets()[1] == 168)              // 192.168.0.0/16
                || (v4.octets()[0] == 169 && v4.octets()[1] == 254)              // 169.254.0.0/16
                || v4.is_unspecified() // 0.0.0.0
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()                           // ::1
                || v6.is_unspecified()                 // ::
                || v6.segments()[0] & 0xfe00 == 0xfc00 // fc00::/7 (unique local)
                || v6.segments()[0] & 0xffc0 == 0xfe80 // fe80::/10 (link-local)
        }
    }
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
            if is_private_ip(&addr.ip()) {
                return Err(Error::Plugin(format!(
                    "allowlisted domain '{domain}' resolved to private/loopback address {}, \
                     which is blocked to prevent SSRF",
                    addr.ip()
                )));
            }
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
    use super::{
        WasmRuntime, is_private_ip, normalize_scoped_path, resolve_allowlisted_ips,
        socket_addr_allowed,
    };
    use crate::manifest::PluginManifest;
    use crate::traits::{Plugin, PluginInput};
    use std::collections::HashSet;
    use std::net::{IpAddr, SocketAddr};
    use std::path::Path;
    use wasmtime_wasi::sockets::SocketAddrUse;

    const ALL_USES: [SocketAddrUse; 7] = [
        SocketAddrUse::TcpBind,
        SocketAddrUse::TcpListen,
        SocketAddrUse::TcpAccept,
        SocketAddrUse::TcpConnect,
        SocketAddrUse::UdpBind,
        SocketAddrUse::UdpSend,
        SocketAddrUse::UdpReceive,
    ];

    fn allowlist() -> HashSet<IpAddr> {
        HashSet::from(["93.184.216.34".parse().unwrap()])
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn socket_policy_allows_remote_traffic_only_to_allowlisted_ips() {
        let allowed = allowlist();
        for reason in [
            SocketAddrUse::TcpConnect,
            SocketAddrUse::UdpSend,
            SocketAddrUse::UdpReceive,
        ] {
            assert!(socket_addr_allowed(
                &allowed,
                addr("93.184.216.34:443"),
                reason
            ));
            assert!(!socket_addr_allowed(&allowed, addr("1.1.1.1:443"), reason));
        }
    }

    #[test]
    fn socket_policy_allows_only_implicit_ephemeral_binds() {
        let allowed = allowlist();
        for reason in [SocketAddrUse::TcpBind, SocketAddrUse::UdpBind] {
            assert!(socket_addr_allowed(&allowed, addr("0.0.0.0:0"), reason));
            assert!(socket_addr_allowed(&allowed, addr("[::]:0"), reason));
            assert!(!socket_addr_allowed(&allowed, addr("0.0.0.0:8080"), reason));
            assert!(!socket_addr_allowed(&allowed, addr("127.0.0.1:0"), reason));
            assert!(!socket_addr_allowed(
                &allowed,
                addr("93.184.216.34:0"),
                reason
            ));
        }
    }

    #[test]
    fn socket_policy_never_allows_listen_or_accept() {
        let allowed = allowlist();
        for reason in [SocketAddrUse::TcpListen, SocketAddrUse::TcpAccept] {
            assert!(!socket_addr_allowed(&allowed, addr("0.0.0.0:0"), reason));
            assert!(!socket_addr_allowed(
                &allowed,
                addr("93.184.216.34:443"),
                reason
            ));
        }
    }

    #[test]
    fn socket_policy_denies_everything_with_empty_allowlist_except_implicit_bind() {
        let empty = HashSet::new();
        for reason in ALL_USES {
            let implicit_bind = matches!(reason, SocketAddrUse::TcpBind | SocketAddrUse::UdpBind);
            assert_eq!(
                socket_addr_allowed(&empty, addr("0.0.0.0:0"), reason),
                implicit_bind
            );
            assert!(!socket_addr_allowed(
                &empty,
                addr("93.184.216.34:443"),
                reason
            ));
        }
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "opencrust-plugin-test-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn resolve_allowlisted_ips_rejects_localhost() {
        let result = resolve_allowlisted_ips(&["localhost".to_string()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("private/loopback") || err.contains("SSRF"));
    }

    #[test]
    fn normalize_scoped_path_creates_writable_path() {
        let root = temp_root("writable");
        let scoped = normalize_scoped_path(Path::new(&root), "rw-data", true).unwrap();
        assert!(scoped.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_traversal_blocked() {
        let root = temp_root("traversal");
        let result = normalize_scoped_path(&root, "../../../etc/passwd", false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes plugin root") || err.contains("does not exist"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn absolute_path_blocked() {
        let root = temp_root("absolute");
        let absolute_path = if cfg!(windows) {
            "C:/Windows/System32/drivers/etc/hosts"
        } else {
            "/etc/passwd"
        };
        let result = normalize_scoped_path(&root, absolute_path, false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("absolute filesystem paths are not allowed")
                || err.contains("does not exist"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn private_ip_rejected() {
        assert!(is_private_ip(&"127.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"172.16.0.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"192.168.1.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"169.254.1.1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"::1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"fd00::1".parse::<IpAddr>().unwrap()));
        assert!(is_private_ip(&"fe80::1".parse::<IpAddr>().unwrap()));
        // Public IP should NOT be private
        assert!(!is_private_ip(&"8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(!is_private_ip(
            &"2001:4860:4860::8888".parse::<IpAddr>().unwrap()
        ));
    }

    // Execution tests: run small WAT modules through the real runtime path
    // (linker, pipes, limits, epoch timeout).

    const WRITE_HELLO: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 16) "hello from wasm\n")
          (func (export "_start")
            (i32.store (i32.const 0) (i32.const 16))
            (i32.store (i32.const 4) (i32.const 16))
            (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 8)))))
    "#;

    fn wasm_plugin(label: &str, wat: &str, limits: &str) -> WasmRuntime {
        let root = temp_root(label);
        let wasm_path = root.join("plugin.wasm");
        std::fs::write(&wasm_path, wat).unwrap();
        let manifest: PluginManifest = toml::from_str(&format!(
            "[plugin]\nname = \"{label}\"\nversion = \"0.1.0\"\ndescription = \"test\"\n\n[limits]\n{limits}\n"
        ))
        .unwrap();
        WasmRuntime::new(manifest, wasm_path).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn executes_plugin_and_captures_stdout() {
        let plugin = wasm_plugin("exec-hello", WRITE_HELLO, "");
        let output = plugin.execute(PluginInput::default()).await.unwrap();
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout, b"hello from wasm\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_exit_code_when_memory_limit_blocks_growth() {
        // Grow by 64 pages (4 MiB) past a 1 MiB cap; exit 3 if growth is refused.
        let wat = r#"
            (module
              (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
              (memory (export "memory") 1)
              (func (export "_start")
                (if (i32.eq (memory.grow (i32.const 64)) (i32.const -1))
                  (then (call $proc_exit (i32.const 3))))))
        "#;
        let plugin = wasm_plugin("exec-memory", wat, "max_memory_mb = 1");
        let output = plugin.execute(PluginInput::default()).await.unwrap();
        assert_eq!(output.status, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn truncates_output_at_limit() {
        // The pipe accepts a short write up to the cap, then reports the
        // stream closed to the guest; output is truncated rather than trapped.
        let plugin = wasm_plugin("exec-output", WRITE_HELLO, "max_output_bytes = 4");
        let output = plugin.execute(PluginInput::default()).await.unwrap();
        assert_eq!(output.stdout, b"hell");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn interrupts_runaway_plugin_after_timeout() {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (func (export "_start") (loop $spin (br $spin))))
        "#;
        let plugin = wasm_plugin("exec-timeout", wat, "timeout_secs = 1");
        let err = plugin
            .execute(PluginInput::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "unexpected error: {err}");
    }
}
