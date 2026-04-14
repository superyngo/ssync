# ssync openssh crate 遷移評估報告

> 評估日期：2026-04-14
> 專案版本：v0.5.0（7,652 行 Rust、63 個測試）
> openssh crate 版本：v0.11.6（2025-12-03 發布）
> 目標：從手工組裝 `Command::new("ssh"/"scp")` 遷移到 openssh crate 的型別安全包裝

---

## 一、openssh crate 概述

### 1.1 核心定位

openssh 是一個 **OpenSSH 的 Rust 包裝層**——它仍然 spawn 系統 `ssh` 程式，但提供：
- 自動 ControlMaster 管理（建立、復用、關閉）
- 型別安全的 `Session` → `Command` → `Child`/`Output` API
- 兩種多工實作：`process-mux`（spawn ssh per command）和 `native-mux`（直接連 control socket）
- 自動 shell escaping
- 配套 `openssh-sftp-client` 提供 SFTP 操作

### 1.2 關鍵特性

| 特性 | 支援度 | 說明 |
|------|-------|------|
| SSH Config 相容性 | ★★★★★ | 完整——底層仍是 ssh binary，所有 config 自動生效 |
| ControlMaster | ★★★★★ | 自動管理，兩種 mux 模式可選 |
| 認證 | ★★★★★ | 完全透傳給 ssh，agent/key/FIDO 全支援 |
| known_hosts | ★★★★★ | 透傳，可選 Strict/Add/Accept |
| ProxyJump | ★★★★★ | SessionBuilder::jump_hosts() |
| SFTP | ★★★★☆ | 透過 openssh-sftp-client，API 完整 |
| Windows | ★☆☆☆☆ | **明確不支援**，文件標註 "only works on unix" |
| 錯誤處理 | ★★★☆☆ | Exit code 255 模糊問題，靠 heuristics 判斷 |
| 並行 channel | ★★★★☆ | 受 sshd MaxSessions 限制（預設 10） |
| 維護狀態 | ★★★★☆ | 活躍開發（267 stars, 37 forks, 17 open issues） |

### 1.3 核心限制

1. **Unix only**：文件明確標註不支援 Windows（ControlMaster 是 Unix 功能）
2. **仍依賴系統 ssh**：PATH 中必須有 `ssh`，binary 不自包含
3. **Exit code 255 歧義**：ssh 自身錯誤和遠端 exit 255 無法區分
4. **不支援 env/cwd 設定**：SSH 協議限制
5. **MaxSessions 限制**：sshd 預設 10 個並行 channel

---

## 二、與 ssync 現有架構的對照分析

### 2.1 現有 SSH 呼叫清單 vs openssh 對應

| ssync 現有操作 | 現有程式碼 | openssh 等效 |
|---------------|-----------|-------------|
| `ssh -o BatchMode=yes <host> -- <cmd>` | `executor::run_remote()` | `session.raw_command(cmd).output()` |
| `ssh + ControlPath` | `executor::run_remote_pooled()` | `session.raw_command(cmd).output()`（自動 ControlMaster） |
| `scp <local> <host>:<remote>` | `executor::upload()` | `Sftp::from_session()` → `fs.write()` |
| `scp <host>:<remote> <local>` | `executor::download()` | `Sftp::from_session()` → `fs.read()` |
| `ssh -o ControlMaster=yes -N -f` | `connection::establish_master()` | `Session::connect_mux()`（自動） |
| `ssh -O exit` | `ConnectionManager::shutdown()` | `session.close()`（自動） |
| scp probe | `executor::scp_probe()` | SFTP `fs.metadata()` 或 `fs.write()` + `fs.remove_file()` |
| shell detect | `shell::detect()` | `session.raw_command("uname -s").output()` |
| `ssh -G <alias>` | `init.rs` | 保留（openssh 不影響此部分） |
| `ssh-keyscan -H` | `init.rs` | 保留（openssh 不影響此部分） |

### 2.2 API 映射細節

**現有 executor::run_remote_pooled()：**
```rust
// 現在（270 行 executor.rs 的核心）
pub async fn run_remote_pooled(
    host: &HostEntry,
    command: &str,
    timeout_secs: u64,
    socket: Option<&Path>,
) -> Result<RemoteOutput> {
    let args = ssh_base_args(host, timeout_secs, socket);
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        Command::new("ssh").args(&args).arg(&host.ssh_host).arg("--").arg(command)
            .stdout(Stdio::piped()).stderr(Stdio::piped()).output(),
    ).await??;
    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        success: output.status.success(),
    })
}
```

**openssh 等效：**
```rust
pub async fn run_remote(
    session: &Session,
    command: &str,
    timeout_secs: u64,
) -> Result<RemoteOutput> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        session.raw_command(command).output(),
    ).await
    .context("SSH connection timeout")?
    .map_err(|e| anyhow::anyhow!("SSH error: {}", e))?;

    Ok(RemoteOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
        success: output.status.success(),
    })
}
```

**注意**：`raw_command()` 不做 escaping（ssync 的指令是已組好的 shell 字串），`command()` 會 escape。ssync 的所有遠端指令都是完整 shell 字串（如 `for f in ...; do ... done`），因此**必須用 `raw_command()`**。

### 2.3 SCP → SFTP 的改動

即使用 openssh crate，SCP 操作仍然要改為 SFTP：

```rust
use openssh_sftp_client::Sftp;

// 從 openssh Session 建立 SFTP
let sftp = Sftp::from_session(session, Default::default()).await?;

// 上傳
sftp.fs().write(remote_path, &local_data).await?;

// 下載
let data = sftp.fs().read(remote_path).await?;
tokio::fs::write(local_path, data).await?;

// 探測（取代 scp_probe）
let exists = sftp.fs().metadata(remote_path).await.is_ok();

// mkdir -p（逐層建立）
sftp.fs().create_dir(parent_path).await?;

// 關閉
sftp.close().await?;
```

但重要的差異是：**這裡的 SFTP 是透過 openssh Session 開出的 sftp subsystem**，底層仍走 ssh binary 的 ControlMaster，而非 russh 那種純 Rust 的 SFTP 實作。

---

## 三、逐模組改動分析

### 3.1 `host/executor.rs`（衝擊：☆☆☆☆）

**改動量：~80% 重寫，但新程式碼更簡潔**

| 函數 | 改動方式 | 行數影響 |
|------|---------|---------|
| `run_remote()` | `session.raw_command(cmd).output()` | 減少 ~50% |
| `run_remote_pooled()` | 合併入 `run_remote()`——不再需要 `_pooled` 版本 | 移除 |
| `upload()` / `upload_pooled()` | SFTP `fs.write()` | 類似行數 |
| `download()` / `download_pooled()` | SFTP `fs.read()` | 類似行數 |
| `scp_probe()` | SFTP `fs.metadata()` 或 write+remove | 簡化 |
| `ssh_base_args()` | **移除**——openssh 自行管理 | 移除 |

**重大簡化**：不再需要 `_pooled` 和非 `_pooled` 兩套函數。openssh 自動管理 ControlMaster，所有函數都自動復用連線。

**executor.rs 的新面貌（概念）：**
```rust
use openssh::Session;
use openssh_sftp_client::Sftp;

pub struct RemoteOutput { /* 不變 */ }

pub async fn run_remote(
    session: &Session,
    command: &str,
    timeout_secs: u64,
) -> Result<RemoteOutput> {
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        session.raw_command(command).output(),
    ).await??;
    // ... 轉換為 RemoteOutput
}

pub async fn upload(
    sftp: &Sftp,
    local_path: &Path,
    remote_path: &str,
    timeout_secs: u64,
) -> Result<()> {
    let data = tokio::fs::read(local_path).await?;
    tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        sftp.fs().write(remote_path, &data),
    ).await??;
    Ok(())
}

pub async fn download(
    sftp: &Sftp,
    remote_path: &str,
    local_path: &Path,
    timeout_secs: u64,
) -> Result<()> {
    let data = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        sftp.fs().read(remote_path),
    ).await??;
    tokio::fs::write(local_path, data).await?;
    Ok(())
}
```

### 3.2 `host/connection.rs`（衝擊：☆☆☆☆☆）

**改動量：~90% 重寫——整個 ControlMaster 管理由 openssh 接管**

**現有 418 行的 ConnectionManager 做的事：**
1. 建立 tempdir for sockets ← openssh 自動做
2. 計算 socket path (blake3 hash) ← openssh 自動做
3. `establish_master()` (ssh -o ControlMaster=yes) ← `Session::connect_mux()` 取代
4. `check_connectivity()` (ssh exit 0) ← `Session::check()` 取代
5. `shutdown()` (ssh -O exit) ← `Session::close()` 取代
6. `Drop` blocking fallback ← openssh 的 Drop 自動處理
7. Socket path 查詢 ← 不再需要

**新 ConnectionManager（概念）：**
```rust
use openssh::{Session, SessionBuilder, KnownHosts};
use openssh_sftp_client::Sftp;
use std::sync::Arc;

pub struct ConnectionManager {
    sessions: HashMap<String, Arc<Session>>,
    sftps: HashMap<String, Arc<Sftp>>,
    scp_failed: HashMap<String, String>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            sftps: HashMap::new(),
            scp_failed: HashMap::new(),
        }
    }

    pub async fn pre_check(
        &mut self,
        hosts: &[&HostEntry],
        timeout_secs: u64,
        concurrency: usize,
    ) -> usize {
        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut handles = Vec::new();

        for host in hosts {
            let sem = semaphore.clone();
            let host = (*host).clone();
            let timeout = Duration::from_secs(timeout_secs);

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let session = tokio::time::timeout(timeout, async {
                    let mut builder = SessionBuilder::default();
                    builder.connect_timeout(timeout);
                    builder.known_hosts_check(KnownHosts::Strict);
                    builder.connect_mux(&host.ssh_host).await
                }).await;
                (host.name.clone(), session)
            }));
        }

        let mut connected = 0;
        for handle in handles {
            match handle.await {
                Ok((name, Ok(Ok(session)))) => {
                    self.sessions.insert(name, Arc::new(session));
                    connected += 1;
                }
                Ok((name, _)) => {
                    // 記錄失敗
                }
                Err(_) => {}
            }
        }
        connected
    }

    pub fn session_for(&self, host_name: &str) -> Option<&Arc<Session>> {
        self.sessions.get(host_name)
    }

    pub async fn get_or_create_sftp(&mut self, host_name: &str) -> Result<&Sftp> {
        if !self.sftps.contains_key(host_name) {
            let session = self.sessions.get(host_name)
                .context("host not connected")?;
            let sftp = Sftp::from_clonable_session(
                session.clone(),
                Default::default(),
            ).await?;
            self.sftps.insert(host_name.to_string(), Arc::new(sftp));
        }
        Ok(self.sftps.get(host_name).unwrap())
    }

    pub async fn shutdown(&mut self) {
        for sftp in self.sftps.drain() {
            let _ = sftp.1.close().await;
        }
        for (_, session) in self.sessions.drain() {
            if let Ok(session) = Arc::try_unwrap(session) {
                let _ = session.close().await;
            }
        }
    }
}
```

**核心簡化**：
- 移除 `ConnectionMode` enum（Pooled/Direct）
- 移除 tempdir/socket 管理
- 移除 `socket_for()` → 改為 `session_for()`
- 移除 Drop 的 blocking ssh -O exit fallback
- 移除 `establish_master()` 和 `check_connectivity()` 兩個私有函數

### 3.3 `host/pool.rs`（衝擊：☆☆☆）

**改動**：
- `socket_for()` → `session_for()` 回傳 `&Arc<Session>`
- `setup_with_options()` 的 SCP probe → SFTP probe
- `ConcurrencyLimiter` 不變
- `SyncProgress` 不變

### 3.4 `host/shell.rs`（衝擊：☆☆☆）

**改動**：`detect()` / `detect_pooled()` 合併為一個版本，使用 Session 而非手工 spawn。

```rust
pub async fn detect(session: &Session, timeout_secs: u64) -> Result<ShellType> {
    // 嘗試 sh
    let output = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        session.raw_command("uname -s 2>/dev/null || echo __NOT_SH__").output(),
    ).await??;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.contains("__NOT_SH__") {
            let trimmed = stdout.trim().to_lowercase();
            if trimmed.contains("linux") || trimmed.contains("darwin") /* ... */ {
                return Ok(ShellType::Sh);
            }
        }
    }
    // ... PowerShell / Cmd 偵測同理
}
```

`sudo_wrap()` 和 `temp_dir()` 完全不受影響。

### 3.5 `host/concurrency.rs`（衝擊：☆）

**完全不受影響**。

### 3.6 `config/ssh_config.rs`（衝擊：☆）

**完全不受影響**。openssh 不需要手動解析 SSH config——底層 ssh binary 自行讀取。現有的 `parse_ssh_config()` 仍用於 `init` 列舉主機名單。

**這是與 russh 方案最大的差異**：russh 需要自行解析所有 SSH config，openssh 完全不需要。

### 3.7 `commands/sync.rs`（衝擊：☆☆☆）

**Stage 1（metadata 收集）**：只改簽名——`run_remote_pooled(host, cmd, timeout, socket)` → `run_remote(session, cmd, timeout)`

**Stage 2（決策）**：完全不受影響。

**Stage 3（分發）**：
- `download_pooled()` → `sftp_download(sftp, remote, local, timeout)`
- `upload_pooled()` → `sftp_upload(sftp, local, remote, timeout)`
- `mkdir -p` → 可用 SFTP `create_dir()` 或保留 `session.raw_command("mkdir -p ...")`

**batch metadata command 建構（build_batch_metadata_cmd 等）**：完全不變——這些是建構遠端 shell 指令字串的邏輯。

### 3.8 `commands/init.rs`（衝擊：☆☆）

- `ssh -G <alias>`：**保留**（openssh 沒有對應功能，這是 config 解析用途）
- `ssh-keyscan -H`：**保留**（openssh 沒有 keyscan 功能）
- `pre_check()`：改用 openssh Session 建立
- `shell::detect_pooled()`：用 openssh Session 執行

**意味著 init.rs 仍需要 `tokio::process::Command`**——但僅限 `ssh -G` 和 `ssh-keyscan` 兩處。

### 3.9 `commands/check.rs`、`run.rs`、`exec.rs`（衝擊：☆☆）

全部只需改簽名：
- `(host, cmd, timeout, socket)` → `(session, cmd, timeout)`
- exec.rs 的 `upload_pooled()` → SFTP upload

### 3.10 `commands/mod.rs`（衝擊：☆☆）

`Context` 可能需要持有 `SshPool` 或 session map 的引用，但改動不大。

### 3.11 `state/db.rs`（衝擊：☆）

完全不受影響——仍然是 blocking rusqlite。

---

## 四、改動量估計

| 模組 | 檔案數 | 現有行數 | 改動比例 | 改動性質 |
|------|-------|---------|---------|---------|
| `host/executor.rs` | 1 | 269 | **80%** | 重寫，但更簡潔（預估 ~150 行） |
| `host/connection.rs` | 1 | 417 | **90%** | Session pool 取代 ControlMaster |
| `host/pool.rs` | 1 | 158 | **40%** | API 簽名改變 |
| `host/shell.rs` | 1 | 269 | **40%** | 合併 detect/detect_pooled |
| `host/concurrency.rs` | 1 | 110 | **0%** | 不受影響 |
| `config/ssh_config.rs` | 1 | 148 | **0%** | 不受影響 |
| `commands/sync.rs` | 1 | 1936 | **15%** | 簽名改變 + Stage 3 SCP→SFTP |
| `commands/init.rs` | 1 | 570 | **15%** | pre_check + detect 簽名改變 |
| `commands/check.rs` | 1 | 260 | **10%** | 簽名改變 |
| `commands/exec.rs` | 1 | 253 | **15%** | upload + exec 簽名改變 |
| `commands/run.rs` | 1 | 107 | **10%** | 簽名改變 |
| `commands/mod.rs` | 1 | 255 | **5%** | Context 持有 pool |
| `state/db.rs` | 1 | ~80 | **0%** | 不受影響 |
| **合計** | 13 | ~4,832 | **~25%** | — |

**與 russh 比較**：
| 面向 | russh 方案 | openssh 方案 |
|------|----------|------------|
| 總改動比例 | ~40%（~3,000 行） | **~25%（~1,500 行）** |
| 新增模組 | 2（auth.rs, sftp.rs） | **0** |
| 新增 dependency | 6（russh, russh-keys, russh-sftp, ssh2-config, ssh-key, async-trait） | **2（openssh, openssh-sftp-client）** |
| SSH config 改動 | 大幅擴充或替換 | **零改動** |
| 認證邏輯 | 全新 ~150 行 | **零改動** |
| known_hosts 邏輯 | 全新 ~50 行 | **零改動** |

---

## 五、新增依賴

```toml
[dependencies]
# 新增
openssh = { version = "0.11", features = ["native-mux"] }
openssh-sftp-client = { version = "0.15", features = ["openssh"] }

# 可移除（部分）
# tokio 的 "process" feature 仍需保留（init.rs 的 ssh -G 和 ssh-keyscan）
```

**Binary size 影響**：極小——openssh 是薄包裝層，不引入密碼學庫。

**Feature 選擇**：
- `process-mux`：每個遠端指令 spawn 一個 ssh process（較安全，兼容性最高）
- `native-mux`：直接連 control socket 的 mux 協議（效能更好，錯誤更精確）
- **建議 `native-mux`**：效能更優且錯誤處理更好；ssync 的使用場景（批量操作）會受益於減少 process spawn

---

## 六、openssh 與 ssync 特有問題的對應

### 6.1 Windows 客戶端支援

**❌ 這是 openssh crate 的最大問題**

openssh 文件明確標註：**"Scriptable SSH through OpenSSH (only works on unix)"**

ssync 現有架構支援 Windows 客戶端（`ConnectionMode::Direct`），openssh 不支援。

**影響評估**：
- ssync 的 **Windows 遠端主機**（PowerShell/Cmd shell）→ **不受影響**（這是遠端，不是客戶端）
- ssync 的 **Windows 客戶端**（從 Windows 執行 ssync）→ **受影響**

**緩解方案**：
1. 保留 `ConnectionMode::Direct` 作為 Windows fallback（feature gate）
2. 或：接受 ssync 不在 Windows 上跑（根據你的 homelab 場景判斷）

### 6.2 MaxSessions 限制

openssh 文件提到：**"the maximum number of multiplexed remote commands is 10 by default"**

ssync 的 `max_per_host_concurrency` 預設 4，一般不會超過 10。但 sync 的 Stage 3 在密集上傳時，如果一台 host 同時有多個 channel（exec + SFTP），可能逼近限制。

**緩解**：ssync 已有 `ConcurrencyLimiter`，確保 per-host 並行度不超過設定值。只要 `max_per_host_concurrency ≤ 8`（留 2 給 SFTP 和監控），就不會有問題。

### 6.3 Exit code 255 歧義

openssh 文件警告：**"we do not have a reliable way to tell the difference between what is a failure of the SSH connection itself, and what is a program error from the remote host"**

ssync 現有代碼已經面對這個問題（ssh binary 本就有此行為），但目前透過 ControlMaster 避免了大部分連線錯誤。遷移到 openssh 的 `native-mux` 模式後，錯誤處理更精確（直接從 mux protocol 取得錯誤），所以**實際上會改善**。

### 6.4 Shell escaping

openssh 預設做 shell escaping（`command()` 方法），但 ssync 的遠端指令已經是完整的 shell 字串。

**⚠️ 必須使用 `raw_command()` 而非 `command()`**——否則 ssync 建構的 `for f in ...; do ... done` 等指令會被錯誤 escape。

### 6.5 SQLite 與 async

與 russh 方案相同的考量，但風險更低——因為 openssh 底層仍是 spawn process，async 壓力較小。

### 6.6 SFTP 大檔案效能

openssh-sftp-client 的 `fs.read()` / `fs.write()` 可能一次載入整個檔案到記憶體。

**對 ssync 的影響**：sync 的檔案通常是 config files（小檔案），但如果有大檔案同步需求，需要用 `sftp.open()` + streaming read/write。

```rust
// 大檔案版本
let mut remote_file = sftp.open(remote_path).await?;
let mut local_file = tokio::fs::File::create(local_path).await?;
tokio::io::copy(&mut remote_file, &mut local_file).await?;
```

---

## 七、風險評估

### 7.1 高風險

| # | 風險 | 嚴重度 | 機率 | 說明 |
|---|------|-------|------|------|
| R1 | Windows 客戶端不支援 | 🔴 | 確定 | openssh 明確不支援 Windows。如 ssync 需要在 Windows 上跑，此為 blocker |
| R2 | ControlMaster hang | 🟡 | 低 | macOS 上有報告 connect_mux 偶爾 hang（issue #150），但已在後續版本修復 |

### 7.2 中風險

| # | 風險 | 嚴重度 | 機率 | 說明 |
|---|------|-------|------|------|
| R3 | MaxSessions 超限 | 🟡 | 低 | 密集操作時可能超過 sshd 預設 10 channel，但 ConcurrencyLimiter 已控制 |
| R4 | openssh Session 非 Clone | 🟡 | 中 | Session 的 close() 消費 self，Arc 包裝時需要 try_unwrap |
| R5 | SFTP 大檔案記憶體 | 🟡 | 低 | fs.read()/write() 是全量載入，需要 streaming 版本處理大檔案 |
| R6 | native-mux agent forwarding | 🟡 | 低 | native-mux + jump_host + agent forwarding 有已知 bug（issue #178） |

### 7.3 低風險

| # | 風險 | 說明 |
|---|------|------|
| R7 | 認證行為 | 完全透傳給 ssh，零風險 |
| R8 | SSH config 相容性 | 完全透傳，零風險 |
| R9 | known_hosts | 完全透傳，零風險 |
| R10 | ProxyJump | SessionBuilder 原生支援 |
| R11 | FIDO/U2F keys | 完全透傳給 ssh，零風險 |
| R12 | 並行模型 | ConcurrencyLimiter 不受影響 |
| R13 | DB 狀態管理 | 不受影響 |

---

## 八、收益評估

### 8.1 確定的收益

| 收益 | 量化影響 |
|------|---------|
| **移除 ControlMaster 手動管理** | 刪除 ~300 行（connection.rs 的核心複雜度）|
| **移除 socket 路徑管理** | 刪除 blake3 hash、tempdir、macOS 104-byte 限制處理 |
| **移除 Drop 的 blocking fallback** | 刪除 blocking ssh -O exit 的安全網 |
| **統一 _pooled / 非 _pooled API** | 從 6 個 executor 函數減為 3 個 |
| **更好的錯誤處理** | openssh::Error 有結構化的 Master/Connect/Remote/Disconnected 分類 |
| **ProxyJump 原生支援** | `SessionBuilder::jump_hosts()` 一行搞定 |
| **100% SSH config 相容性** | 不需要自行解析任何額外欄位 |
| **保留現有認證行為** | agent/key/FIDO/certificate 全部自動工作 |
| **程式碼量淨減少** | 預估淨減 ~500 行 |

### 8.2 相比 russh 方案的獨特優勢

| 面向 | russh | openssh |
|------|-------|---------|
| SSH config 改動 | 大幅擴充或替換解析器 | **零改動** |
| 認證邏輯 | 新寫 ~150 行（agent→key fallback） | **零改動** |
| known_hosts | 新寫 ~50 行 | **零改動** |
| FIDO key 支援 | ❌ 不支援 | **✅ 自動支援** |
| ProxyJump | 需自行實作多層跳板 | **一行設定** |
| 遷移風險 | 高（行為可能與 ssh 不一致） | **低（底層仍是 ssh）** |
| 偵錯方式 | 自行 tracing | **可搭配 ssh -vvv 排查** |
| 「ssh 能連但 ssync 不能」的問題 | 可能發生 | **極不可能** |

### 8.3 代價

| 代價 | 說明 |
|------|------|
| 仍依賴系統 ssh | binary 不自包含，部署環境需有 OpenSSH |
| Windows 客戶端不支援 | 如果需要，必須保留 fallback |
| SCP → SFTP 仍需改動 | 但這是兩個方案共有的 |
| process spawn 開銷仍在 | native-mux 模式下已大幅降低，但不是零 |

---

## 九、遷移策略

### Phase 1：executor 層替換（1-2 天）

1. 新增 openssh / openssh-sftp-client 依賴
2. 重寫 `host/connection.rs`：
   - `ConnectionManager` 持有 `HashMap<String, Arc<Session>>`
   - `pre_check()` 改為 `Session::connect_mux()` 批次連線
   - 移除所有 socket/tempdir 管理
   - `shutdown()` 改為 `session.close()` 逐一關閉
3. 重寫 `host/executor.rs`：
   - 移除所有 `_pooled` 函數
   - `run_remote(session, cmd, timeout)` 用 `session.raw_command(cmd).output()`
   - `upload()` / `download()` 用 openssh-sftp-client
4. 更新 `host/pool.rs`：
   - `socket_for()` → `session_for()`
   - SCP probe → SFTP probe

### Phase 2：command 層適配（1-2 天）

1. 所有 command 模組更新函數簽名
2. sync.rs Stage 3：SCP → SFTP
3. exec.rs：upload_pooled → SFTP upload
4. init.rs：保留 `ssh -G` 和 `ssh-keyscan`（仍用 tokio::process::Command）

### Phase 3：清理與測試（1 天）

1. 移除不再需要的 `_pooled` 版本函數
2. 移除 `ConnectionMode` enum（或保留 Direct 做 Windows fallback）
3. 更新所有測試
4. `cargo test && cargo clippy && cargo build --no-default-features`

### Phase 4（可選）：Windows fallback（1 天）

如果需要 Windows 客戶端支援：
```rust
pub struct ConnectionManager {
    #[cfg(not(target_os = "windows"))]
    inner: OpensshPool,  // 使用 openssh crate
    #[cfg(target_os = "windows")]
    inner: DirectPool,   // 保留現有 spawn ssh 行為
}
```

### 總預估工期：3-5 天

（對比 russh 的 11-17 天）

---

## 十、邊界情況清單

### 10.1 openssh 特有邊界

- [ ] `raw_command()` vs `command()`——ssync 的所有指令必須用 `raw_command()`
- [ ] `Session::close()` 消費 self——Arc 包裝時的 try_unwrap 問題
- [ ] SFTP 的 `~` 路徑展開——需要先 `fs.canonicalize("~")` 取得 home dir
- [ ] SFTP 不自動建立中間目錄——`fs.write()` 會失敗如果目錄不存在
- [ ] `native-mux` 模式下 agent forwarding + jump host 的已知 bug
- [ ] ControlMaster socket 清理——openssh 的 `.ssh-connection*` 臨時目錄

### 10.2 SFTP 邊界（兩方案共有）

- [ ] 符號連結跟隨策略
- [ ] 檔案權限保持（`fs.set_permissions()`）
- [ ] 大檔案 streaming（避免一次性載入記憶體）
- [ ] 遠端磁碟已滿的錯誤處理
- [ ] Windows 遠端的路徑分隔符

### 10.3 不需要擔心的（openssh 自動處理）

- [x] SSH config 的 Match/Include/CanonicalizeHostname
- [x] SSH Agent 認證
- [x] IdentityFile 路徑展開（`~`, `%h` 等）
- [x] 加密私鑰 passphrase（BatchMode=yes 行為）
- [x] FIDO/U2F key 認證
- [x] Certificate-based auth
- [x] known_hosts 驗證
- [x] ProxyJump / ProxyCommand
- [x] IPv6 literal address

---

## 十一、與 russh 方案的最終比較

| 面向 | russh | openssh | 勝者 |
|------|-------|---------|------|
| 改動量 | ~40%（~3,000 行） | ~25%（~1,500 行） | **openssh** |
| 工期 | 11-17 天 | 3-5 天 | **openssh** |
| SSH config 相容性 | ★★★ | ★★★★★ | **openssh** |
| 認證相容性 | ★★★（無 FIDO） | ★★★★★ | **openssh** |
| known_hosts | 需自行實作 | 自動 | **openssh** |
| ProxyJump | 需自行實作 | 原生支援 | **openssh** |
| Windows 客戶端 | ★★★★★ | ★☆☆☆☆ | **russh** |
| Binary 自包含 | ★★★★★ | ★☆☆☆☆ | **russh** |
| Process spawn 開銷 | 零 | 有（native-mux 降低） | **russh** |
| 偵錯便利性 | ★★ | ★★★★★ | **openssh** |
| 遷移風險 | 高 | 低 | **openssh** |
| 維護成本 | 高（自行管理 SSH） | 低（透傳給 ssh） | **openssh** |
| 安全責任 | 自行承擔 | 交給 OpenSSH | **openssh** |

---

## 十二、結論

### 核心判斷

**遷移的技術可行性：✅ 非常高**——openssh crate 的 API 與 ssync 現有架構高度匹配。

**遷移的必要性：✅ 中等偏高**——主要收益是移除 ControlMaster 手動管理這個最複雜的模組，同時獲得更好的錯誤處理和 ProxyJump 支援。

**遷移的 ROI（Unix-only 場景）：✅ 明確正向**——以 ~1,500 行改動換取 ~500 行淨減 + 大幅降低 `host/connection.rs` 的複雜度 + 保留 100% SSH 相容性。

### 決策建議（Windows 為必要需求時）

> **更新（2026-04-14）**：確認 Windows 客戶端支援為必要需求。

**openssh 方案被排除**——它是 Unix-only 硬限制。用 feature-gate 維護兩套 SSH 後端（openssh on Unix + Direct spawn on Windows）的長期維護成本會抵消收益。

**最終建議：維持現狀。**

理由：
1. 現有 spawn ssh 架構已可靠運作，Windows 有 `ConnectionMode::Direct` 支援
2. **russh** 的 ~3,000 行重寫（40% codebase）風險高：FIDO key 不支援、SSH config 需自行解析、exit code 取得需重新實作
3. **openssh + Windows fallback** 等於維護兩套 SSH 後端，複雜度不降反升
4. 除非有明確的「消除外部 ssh 依賴」需求（如打包為單一 binary 部署到無 ssh 的環境），否則遷移的 ROI 不划算

**如果未來需要改善 SSH 層**，建議的漸進路線：
1. 先將 `host/executor.rs` 和 `host/connection.rs` 抽象為 trait（`SshTransport`）
2. 現有 spawn 實作作為 `ProcessTransport`
3. 未來如有需要，可加入 `RusshTransport` 作為可選後端
4. 這個 trait 抽象本身就是有價值的重構，即使不遷移也提升了可測試性
