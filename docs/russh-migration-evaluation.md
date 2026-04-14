# ssync russh 遷移評估報告

> 評估日期：2026-04-14
> 專案版本：v0.5.0（7,652 行 Rust、63 個測試）
> 目標：從 spawn `ssh`/`scp` 程式遷移到 russh 嵌入式 SSH 客戶端

---

## 一、現有架構概述

### 1.1 SSH 傳輸層面

ssync 目前以 **黑盒子模式** 使用系統 `ssh`/`scp`：

| 操作             | 實作方式                              | 呼叫處                                                       |
|-----------------|--------------------------------------|-------------------------------------------------------------|
| 遠端指令執行      | `ssh -o BatchMode=yes <host> -- <cmd>` | `executor::run_remote()`, `run_remote_pooled()`             |
| 檔案上傳         | `scp <local> <host>:<remote>`          | `executor::upload()`, `upload_pooled()`                     |
| 檔案下載         | `scp <host>:<remote> <local>`          | `executor::download()`, `download_pooled()`                 |
| 連線探測         | `ssh <host> exit 0`                    | `connection::check_connectivity()`                           |
| ControlMaster   | `ssh -o ControlMaster=yes -N -f`       | `connection::establish_master()`                             |
| ControlMaster 關閉 | `ssh -O exit`                        | `ConnectionManager::shutdown()`, `Drop`                     |
| SCP 探測         | 上傳+刪除 1-byte 探針檔                  | `executor::scp_probe()`                                     |
| Shell 偵測       | 嘗試 `uname -s`→`powershell`→`ver`     | `shell::detect()`, `detect_pooled()`                        |
| Host key 掃描    | `ssh-keyscan -H -p <port> <host>`      | `commands/init.rs::batch_keyscan_and_accept()`              |
| SSH config 解析  | `ssh -G <alias>`（init 時）             | `commands/init.rs::resolve_ssh_host_port()`                 |

**共 15 個不同的 `Command::new("ssh")`/`"scp"`/`"ssh-keyscan"` 呼叫點**，分布在 4 個模組中。

### 1.2 連線池架構

```
ConnectionManager
├── Pooled mode (Unix)
│   ├── tempdir /tmp/ssync-XXXXXX/
│   ├── per-host socket: blake3(hostname)[0..12]
│   ├── establish_master() → ssh -o ControlMaster=yes -N -f
│   └── shutdown() → ssh -O exit 逐一關閉
└── Direct mode (Windows)
    └── check_connectivity() → ssh exit 0
```

`SshPool` 將 `ConnectionManager` + `ConcurrencyLimiter`（雙層信號量：global + per-host）+ `SyncProgress` 包裝為統一入口。

### 1.3 三層並行模型

```
全域 Semaphore (max_concurrency=10)
  └── per-host Semaphore (max_per_host_concurrency=4)
       └── tokio::spawn per host operation
```

每個 spawn 出去的 task 裡呼叫 `Command::new("ssh"/"scp")`，各自是獨立 process，天然隔離。

### 1.4 SSH Config 依賴

ssync 自行解析 `~/.ssh/config` 只取 5 個欄位（`Host`, `HostName`, `User`, `Port`, `IdentityFile`），其餘全靠 ssh binary 自行處理。init 時呼叫 `ssh -G <alias>` 讓 ssh 自己展開最終設定。

---

## 二、russh 遷移影響的逐模組分析

### 2.1 `host/executor.rs`（核心衝擊：☆☆☆☆☆）

**現狀**：6 個 public 函數，每個都是 spawn `ssh`/`scp` + timeout 包裝。

**遷移影響**：

| 函數                    | 改動方式                                                           |
|------------------------|-------------------------------------------------------------------|
| `run_remote()`          | → 開 channel → `channel.exec(cmd)` → 讀 stdout/stderr → 捕捉 ExitStatus |
| `run_remote_pooled()`   | → 複用已有 Session 的 channel（取代 ControlPath 參數）                  |
| `upload()`/`upload_pooled()` | → `russh_sftp::client::SftpSession` → `create()` → `write()` |
| `download()`/`download_pooled()` | → SFTP `open()` → `read()` → 寫入 local file            |
| `scp_probe()`           | → SFTP `stat()` 或嘗試 `create()`+`remove()` 即可                   |

**關鍵差異**：

```rust
// 現在（spawn process，exit code 直接從 Output 取）
let output = Command::new("ssh").args(&args).output().await?;
let exit_code = output.status.code(); // Option<i32>，直接可用

// russh（exit code 藏在 channel event stream 裡）
let mut channel = session.channel_open_session().await?;
channel.exec(true, cmd).await?;
let mut exit_code: Option<u32> = None;
let mut stdout = Vec::new();
let mut stderr = Vec::new();
loop {
    match channel.wait().await {
        Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
        Some(ChannelMsg::ExtendedData { data, ext }) if ext == 1 =>
            stderr.extend_from_slice(&data),
        Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = Some(exit_status),
        Some(ChannelMsg::Eof) | None => break,
        _ => {}
    }
}
// ⚠️ ExitStatus 可能在 Eof 之前或之後到達，必須 drain 到 None
```

**風險**：
- ExitStatus 在 channel drain loop 中容易漏接（如提前 break）
- 大量 stdout 時需要設定 window size 避免阻塞
- stderr 是 extended data (type=1)，不是獨立 pipe

### 2.2 `host/connection.rs`（核心衝擊：☆☆☆☆☆）

**現狀**：ControlMaster + socket 管理，Unix/Windows 雙模式。

**遷移後**：ControlMaster 概念完全消失，改為 russh `Session` 池。

```rust
// 遷移後的概念設計
pub struct ConnectionManager {
    sessions: HashMap<String, Arc<russh::client::Handle<SshHandler>>>,
    // 不再需要 socket_dir, socket_path, host_map
}
```

**需要處理**：
- `Session`（`Handle`）不是 `Clone`，但 `Handle` 在 russh 中是 `Arc`-wrapped 的，多個 channel 可從同一 Handle 開出
- `shutdown()` 從 `ssh -O exit` 改為 `handle.disconnect()`
- `Drop` 實作：russh 的 `Handle` drop 會自動關閉底層 TCP，不再需要 blocking fallback
- `socket_for()` API 整個移除——由 `session_for()` 取代

**連帶影響**：所有呼叫 `socket_for()` / `socket.as_deref()` 的 command 模組都要改簽名。

### 2.3 `host/pool.rs`（衝擊：☆☆☆☆）

**現狀**：封裝 `ConnectionManager` + `ConcurrencyLimiter` + `SyncProgress`。

**遷移後**：
- `setup()` 改為建立 russh Session 池，不再分 Pooled/Direct 模式
- `socket_for()` → `session_for()` 回傳 `Arc<Handle<SshHandler>>`
- `filter_scp_capable()` → SFTP 子系統探測取代 SCP probe
- `ConcurrencyLimiter` 邏輯不變（仍然需要 global + per-host 信號量）

### 2.4 `host/shell.rs`（衝擊：☆☆☆）

**現狀**：3 次獨立 ssh 呼叫偵測 shell 類型。

**遷移後**：用已建立的 Session 開 channel 執行偵測指令。邏輯不變，只是底層從 spawn 改為 channel.exec()。但要注意：偵測失敗時 channel 可能異常關閉，需要妥善處理 `ChannelOpenFailure`。

### 2.5 `host/concurrency.rs`（衝擊：☆）

**完全不受影響**。雙層信號量是邏輯層控制，與傳輸無關。

### 2.6 `config/ssh_config.rs`（衝擊：☆☆☆☆☆）

**現狀**：手動解析 5 個欄位，其餘靠 ssh binary。

**遷移後**：ssync **必須自己理解所有需要的 SSH config 欄位**，因為不再有 ssh binary 幫你解析。

需要新增解析的欄位：

| 欄位 | 現狀 | russh 需要 | 用途 |
|------|------|-----------|------|
| `Host` | ✅ 已解析 | ✅ | 主機別名 |
| `HostName` | ✅ 已解析 | ✅ | 實際連線地址 |
| `User` | ✅ 已解析 | ✅ | 認證用戶名 |
| `Port` | ✅ 已解析 | ✅ | TCP port |
| `IdentityFile` | ✅ 已解析 | ✅ | 私鑰路徑 |
| `ProxyJump` | ❌ 未解析 | ⚠️ 視需求 | 跳板機（見 §三.5） |
| `ProxyCommand` | ❌ 未解析 | ⚠️ 視需求 | 自訂代理指令 |
| `IdentitiesOnly` | ❌ 未解析 | ✅ | 是否僅用指定 key |
| `ServerAliveInterval` | ❌ 未解析 | ✅ | Keep-alive |
| `ServerAliveCountMax` | ❌ 未解析 | ✅ | Keep-alive 最大次數 |
| `ConnectTimeout` | ❌ (用 CLI arg) | ✅ | 連線超時 |
| `StrictHostKeyChecking` | ❌ 未解析 | ⚠️ | known_hosts 行為 |
| `UserKnownHostsFile` | ❌ 未解析 | ⚠️ | 自訂 known_hosts 路徑 |
| `Match` | ❌ 跳過 | ❌ 極困難 | 條件式設定 |
| `Include` | ❌ 跳過 | ⚠️ | 引入其他 config 檔 |

**建議**：使用 `ssh2-config` crate（已穩定，支援 `Match`/`Include`/通配符展開），而非自行擴充解析器。`ssh2-config` 可以：
- 解析完整 `~/.ssh/config`（含 Include、Match）
- 依主機名稱查詢最終合併後的設定
- 回傳所有上述欄位

**代價**：新增一個 dependency，但省去大量解析邏輯。現有的 `parse_ssh_config()` 仍可保留用於 `init` 時列舉主機名單。

### 2.7 `commands/init.rs`（衝擊：☆☆☆☆）

**需要改動的地方**：
- `ssh -G <alias>` 解析（Line 33-38）→ 改用 `ssh2-config` 查詢 HostName/Port
- `ssh-keyscan -H`（Line 64-76）→ 這個**沒有 russh 替代方案**
  - 選項 A：保留 `ssh-keyscan` 作為唯一外部依賴（推薦）
  - 選項 B：用 russh 連線時從 `check_server_key` callback 取得 server key，自行格式化寫入 known_hosts
- `pre_check()` → 改用 russh Session 建立
- `shell::detect_pooled()` → 用 russh channel 執行

### 2.8 `commands/sync.rs`（衝擊：☆☆☆☆）

1936 行的核心同步邏輯。主要改動點：

| 階段 | 現狀 | 遷移後 |
|------|------|--------|
| Stage 1：metadata 收集 | `run_remote_pooled()` 跑 batch shell 指令 | 改用 channel.exec()，**指令字串不變** |
| Stage 3：下載 | `scp host:path local` | SFTP `open()` + `read()` |
| Stage 3：上傳 | `scp local host:path` | SFTP `create()` + `write()` |
| Stage 3：mkdir | `run_remote_pooled("mkdir -p ...")` | SFTP `create_dir()` 或保留 channel.exec() |

**關鍵觀察**：Stage 1 和 Stage 2 幾乎不需要改動（它們是指令字串建構+解析），只有底層傳輸方式改變。Stage 3 的 SCP→SFTP 是最大改動。

### 2.9 `commands/check.rs`、`run.rs`、`exec.rs`（衝擊：☆☆☆）

這三個都是「在遠端跑指令」的模式，只需要：
- `run_remote_pooled()` 改簽名（Session handle 取代 socket path）
- exec.rs 的 `upload_pooled()` 改為 SFTP

### 2.10 `state/db.rs`（衝擊：☆☆）

**SQLite 與 async 的邊界問題**——這不是 russh 直接引入的，但遷移時值得一併檢視。

**現狀**：rusqlite 是 blocking，直接在 async context 中呼叫。目前可接受是因為 DB 操作極快且 WAL 模式下讀寫分離。

**遷移後的風險**：russh 的 channel I/O 完全在 tokio runtime 裡，如果某個 task 在 `run_remote()` → 寫 DB → `run_remote()` 的 chain 中呼叫 blocking DB，會佔住 tokio worker thread。

**建議**：
- 短期：維持現狀（DB 操作 < 1ms，實務上不會造成問題）
- 長期：將 `conn.execute()` 包在 `tokio::task::spawn_blocking()` 裡
- 或改用 `tokio-rusqlite`（async wrapper）

---

## 三、核心設計決策

### 3.1 認證流程：從隱式變顯式

**現狀**：ssh binary 自動嘗試 `agent → identity_file → password → keyboard-interactive`。

**遷移後必須自行實作認證序列**：

```rust
async fn authenticate(session: &mut Handle<SshHandler>, user: &str, config: &HostConfig) -> Result<()> {
    // 1. 嘗試 SSH Agent
    if let Ok(agent_sock) = std::env::var("SSH_AUTH_SOCK") {
        // 連接 agent，列出 key，逐一嘗試
        if try_agent_auth(session, user, &agent_sock).await? {
            return Ok(());
        }
    }

    // 2. 嘗試 IdentityFile
    for key_path in &config.identity_files {
        let expanded = expand_tilde(key_path);
        if let Ok(key) = russh_keys::load_secret_key(&expanded, None) {
            if session.authenticate_publickey(user, Arc::new(key)).await? {
                return Ok(());
            }
        }
        // 可能需要 passphrase → 互動或 bail
    }

    // 3. 不支援 password（BatchMode=yes 的語意）
    bail!("Authentication failed for {}", user)
}
```

**邊界情況**：
- 加密私鑰（需 passphrase）：ssync 目前用 `BatchMode=yes`，不支援互動輸入。遷移後同樣應 bail
- SSH Agent forwarding：russh 支援，但 ssync 不需要
- Certificate-based auth：russh 支援 `authenticate_publickey` 搭配 cert，但需額外處理 `-cert.pub` 檔案
- FIDO/U2F keys（ed25519-sk, ecdsa-sk）：**russh 目前不支援 FIDO key 認證**——這是 blocker 如果你的 homelab 用 hardware key

### 3.2 known_hosts 驗證

**必須實作**，不能跳過。russh 透過 `Handler` trait 的 `check_server_key` callback：

```rust
#[async_trait]
impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // 使用 russh::client::known_hosts::check_known_hosts_path
        // 對照 ~/.ssh/known_hosts
        let known_hosts_path = dirs::home_dir()
            .map(|h| h.join(".ssh/known_hosts"))
            .context("cannot find home")?;
        match russh::client::check_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &known_hosts_path,
        ) {
            Ok(true) => Ok(true),
            Ok(false) => {
                // Key mismatch → MITM warning
                bail!("HOST KEY VERIFICATION FAILED for {}", self.host)
            }
            Err(_) => {
                // Not in known_hosts → 根據 StrictHostKeyChecking 決定
                // ssync 的 init 流程已處理 keyscan，這裡應該 bail
                bail!("Unknown host key for {}", self.host)
            }
        }
    }
}
```

**邊界**：
- 非標準 known_hosts 路徑（`UserKnownHostsFile`）需要從 SSH config 讀取
- hashed known_hosts（`HashKnownHosts`）：russh 的 `check_known_hosts_path` 支援
- 多個 known_hosts 檔案：OpenSSH 支援多個路徑，需要逐一檢查

### 3.3 Session 共享模型

**問題**：russh `Handle` 可以從同一連線開多個 channel，但有 API 限制。

**建議方案**：一個 host 一個 Session，多個 channel 從同一 Session 開出。

```rust
// 概念設計
pub struct SessionPool {
    sessions: HashMap<String, Arc<russh::client::Handle<SshHandler>>>,
}

impl SessionPool {
    pub async fn get_or_connect(&mut self, host: &HostEntry) -> Result<Arc<Handle<SshHandler>>> {
        if let Some(handle) = self.sessions.get(&host.name) {
            if handle.is_closed() {
                self.sessions.remove(&host.name);
            } else {
                return Ok(handle.clone());
            }
        }
        let handle = self.connect(host).await?;
        let handle = Arc::new(handle);
        self.sessions.insert(host.name.clone(), handle.clone());
        Ok(handle)
    }
}
```

**russh 的 Handle 是否支援並行 channel？**
- 是，`Handle` 內部用 `mpsc` channel 與底層 session task 通訊，可從多個 tokio task 同時開 channel
- 不需要 `Mutex`——`Handle` 的方法已經是 async-safe
- 但 **SFTP subsystem 可能只能開一個**——需要 SFTP channel pool 或 per-transfer channel

### 3.4 SFTP 取代 SCP

**SCP 與 SFTP 的概念差異**：

| 面向 | SCP（現狀） | SFTP（遷移後） |
|------|-----------|--------------|
| 協議 | 舊式 RCP-over-SSH | 子系統，類似遠端 FS API |
| 目錄建立 | scp 會自動建立中間目錄 | 需要 `create_dir()` 逐層建立 |
| 權限保持 | scp -p 保持 mtime/mode | SFTP `setstat()` 明確設定 |
| 大檔案 | 外部 process streaming | 需要手動分塊 read/write |
| 並行傳輸 | 每個 scp 是獨立 process | 每個傳輸是獨立 channel |

**russh-sftp 用法**：

```rust
use russh_sftp::client::SftpSession;

// 從已有 Session 開啟 SFTP
let channel = session.channel_open_session().await?;
channel.request_subsystem(true, "sftp").await?;
let sftp = SftpSession::new(channel.into_stream()).await?;

// 上傳
let mut remote_file = sftp.create(remote_path).await?;
let local_data = tokio::fs::read(local_path).await?;
remote_file.write_all(&local_data).await?;
remote_file.shutdown().await?;

// 下載
let mut remote_file = sftp.open(remote_path).await?;
let mut local_file = tokio::fs::File::create(local_path).await?;
tokio::io::copy(&mut remote_file, &mut local_file).await?;
```

**影響 sync.rs 的點**：
- `upload_pooled()` → SFTP write
- `download_pooled()` → SFTP read
- `scp_probe()` → SFTP `stat()` 或 `create()`+`remove()`
- mkdir 建立中間目錄 → SFTP `create_dir()` 逐層

### 3.5 ProxyJump 支援

**這是架構層面的決策**，需要在遷移前確定。

**方案 A：不支援 ProxyJump（假設 Tailscale 直連）**
- 最簡實作
- 如果所有 host 都在 Tailscale 內，這就夠了
- 未來需要時再加

**方案 B：支援 ProxyJump**
- russh 沒有內建 ProxyJump
- 需要自行實作：先 SSH 到跳板機 → 開 direct-tcpip channel → 在此 channel 上建立第二層 SSH
- 遞迴支持多層 ProxyJump 會增加大量複雜度

```rust
// ProxyJump 的概念實作
async fn connect_via_proxy(
    proxy_host: &HostEntry,
    target_host: &str,
    target_port: u16,
) -> Result<Handle<SshHandler>> {
    // 1. 連線到跳板機
    let proxy_session = connect(proxy_host).await?;

    // 2. 開 direct-tcpip channel
    let channel = proxy_session.channel_open_direct_tcpip(
        target_host, target_port as u32,
        "127.0.0.1", 0,
    ).await?;

    // 3. 在此 channel 上建立新 SSH session
    let config = Arc::new(russh::client::Config::default());
    let handler = SshHandler::new(target_host, target_port);
    let session = russh::client::connect_stream(config, channel.into_stream(), handler).await?;

    Ok(session)
}
```

**建議**：Phase 1 不支援。在 config 中加入明確提示：若 host 需要 ProxyJump，暫時不支援內嵌 SSH，或提供 fallback 到 spawn ssh 的選項。

---

## 四、遷移影響矩陣

### 4.1 模組改動量估計

| 模組 | 檔案數 | 現有行數 | 改動比例 | 改動性質 |
|------|-------|---------|---------|---------|
| `host/executor.rs` | 1 | 269 | **95%** | 完全重寫 |
| `host/connection.rs` | 1 | 417 | **90%** | Session pool 取代 ControlMaster |
| `host/pool.rs` | 1 | 158 | **70%** | API 簽名改變 |
| `host/shell.rs` | 1 | 269 | **60%** | spawn ssh → channel.exec() |
| `host/concurrency.rs` | 1 | 110 | **0%** | 不受影響 |
| `config/ssh_config.rs` | 1 | 148 | **40%** | 可能改用 ssh2-config |
| `commands/sync.rs` | 1 | 1936 | **30%** | SCP→SFTP，API 簽名改變 |
| `commands/init.rs` | 1 | 570 | **40%** | ssh -G/keyscan 改法 |
| `commands/check.rs` | 1 | 260 | **15%** | 簽名改變 |
| `commands/exec.rs` | 1 | 253 | **25%** | upload+exec 改法 |
| `commands/run.rs` | 1 | 107 | **15%** | 簽名改變 |
| `commands/mod.rs` | 1 | 255 | **10%** | Context 可能需持有 session pool |
| `state/db.rs` | 1 | ~80 | **0-5%** | 可選的 spawn_blocking 包裝 |
| 新增：`host/auth.rs` | 1 | ~150(新) | **100%** | 認證邏輯（新模組） |
| 新增：`host/sftp.rs` | 1 | ~200(新) | **100%** | SFTP 操作封裝 |
| **合計** | 15 | ~4,782 | **~40%** | — |

### 4.2 新增依賴

```toml
[dependencies]
# SSH
russh = "0.46"           # SSH protocol
russh-keys = "0.46"      # Key loading/agent
russh-sftp = "2.0"       # SFTP client
ssh2-config = "0.3"      # SSH config parser (取代自行解析)
ssh-key = "0.6"          # Key types
async-trait = "0.1"      # Handler trait
```

**移除**：不再需要 `tokio::process::Command` 的 `process` feature（但 init.rs 的 keyscan 仍需要，除非完全移除外部指令）。

### 4.3 Binary size 影響

- 現狀：ssync 不含 crypto 依賴，binary 較小
- russh 帶入：`ring` 或 `rust-crypto`（密碼學）、`ssh-encoding`、`ssh-key`
- 預估增加：+2-4 MB（release build）
- 交叉編譯：russh 使用 `ring`（有平台限制）或可選 `aws-lc-rs`

---

## 五、風險評估

### 5.1 高風險項目

| # | 風險 | 嚴重度 | 機率 | 說明 |
|---|------|-------|------|------|
| R1 | FIDO/U2F key 不支援 | 🔴 | 中 | russh 不支援 ed25519-sk/ecdsa-sk。如果任何 host 使用 hardware key，認證會失敗 |
| R2 | SSH config 相容性缺口 | 🔴 | 高 | `Match`/`Include`/`CanonicalizeHostname` 等進階 directive 難以完整支援 |
| R3 | Exit code 遺漏 | 🟡 | 中 | Channel event stream 的 ExitStatus 容易漏接，導致 ssync 狀態記錄出錯 |
| R4 | SFTP 大檔案效能 | 🟡 | 中 | russh-sftp 的 streaming 效能可能不如系統 scp（需要 benchmark） |
| R5 | 平台差異 | 🟡 | 低 | russh 在 Windows 上的 SSH Agent 支援（named pipe vs Unix socket）需要驗證 |
| R6 | 加密私鑰 passphrase | 🟡 | 中 | 目前 BatchMode=yes 不需處理，但 russh 載入加密 key 時會失敗，需要明確跳過 |

### 5.2 中風險項目

| # | 風險 | 嚴重度 | 機率 | 說明 |
|---|------|-------|------|------|
| R7 | Session 斷線重連 | 🟡 | 中 | 長時間 sync 時 TCP 連線可能中斷，需要重連邏輯（現在 ControlMaster 自動處理） |
| R8 | Channel window size | 🟡 | 低 | 預設 window size 可能不足，導致大量資料傳輸時 throughput 下降 |
| R9 | SSH Agent timeout | 🟡 | 低 | Agent 連線可能 timeout，需要 fallback 到 IdentityFile |
| R10 | russh crate 維護狀態 | 🟡 | 低 | russh 活躍維護中（Warp 團隊），但非 OpenSSH 級別的成熟度 |

### 5.3 低風險項目

| # | 風險 | 說明 |
|---|------|------|
| R11 | 雙層並行模型 | ConcurrencyLimiter 完全不受影響 |
| R12 | Shell probe 邏輯 | 指令字串不變，只改底層傳輸 |
| R13 | 指標收集/解析 | metrics 模組的 probe 指令和 parser 完全不受影響 |
| R14 | TUI 功能 | 與傳輸層無關 |
| R15 | SQLite 狀態管理 | 與傳輸層無關 |

---

## 六、收益評估

### 6.1 確定的收益

| 收益 | 說明 |
|------|------|
| 移除外部依賴 | 不再要求系統安裝 `ssh`/`scp`，降低部署門檻（尤其 Windows） |
| 減少 process spawn 開銷 | 每次 SSH 操作不再 fork+exec，在大量 host/file 時顯著減少 overhead |
| 更精細的連線控制 | 可以精確管理 keep-alive、重連、channel 多工 |
| 統一錯誤處理 | 不再需要解析 stderr 文字來判斷錯誤類型（russh 提供結構化錯誤） |
| 避免 ControlMaster 複雜性 | 不再需要 socket 路徑管理、macOS 104-byte 限制、Drop 的 blocking fallback |
| 二進位自包含 | 單一 binary 完全獨立運行，不依賴 PATH 裡的 ssh |

### 6.2 不確定/可能的收益

| 收益 | 條件 |
|------|------|
| 效能提升 | 取決於 russh 的 crypto 實作效能 vs 系統 OpenSSH |
| 更好的 Windows 體驗 | 取決於 russh 在 Windows 上的成熟度 |
| 更好的 CI 整合 | 不需要在 CI 環境安裝 ssh，但 ssync 主要是 CLI 工具而非 library |

### 6.3 代價

| 代價 | 說明 |
|------|------|
| 大量改動 | 估計改動 ~40% 的程式碼（~3,000 行） |
| SSH config 相容性下降 | 不可能 100% 複製 OpenSSH 的所有 config 處理 |
| 認證行為差異 | 用戶可能遇到「ssh 能連但 ssync 不能」的情況 |
| 偵錯困難 | 出問題時不能用 `ssh -vvv` 對比排查 |
| 維護負擔增加 | 認證/config/known_hosts 都變成 ssync 的責任 |
| 安全表面擴大 | 自行實作 SSH 客戶端 = 自行承擔所有密碼學風險 |

---

## 七、替代方案

### 7.1 方案比較

| 方案 | 改動量 | 相容性 | 維護成本 | 部署便利性 |
|------|-------|--------|---------|-----------|
| A. 維持現狀（spawn ssh/scp） | 0 | ★★★★★ | ★★★★★ | ★★★ |
| B. 全面遷移 russh | 大 | ★★★ | ★★★ | ★★★★★ |
| C. 混合模式：russh 為主，fallback spawn ssh | 大 | ★★★★ | ★★ | ★★★★ |
| D. 改用 openssh crate（spawn ssh 的型別安全包裝） | 小 | ★★★★★ | ★★★★ | ★★★ |
| E. Feature gate：`--features=embedded-ssh` | 大 | ★★★★★ | ★★★ | ★★★★ |

### 7.2 方案 D 值得考慮

[`openssh`](https://crates.io/crates/openssh) crate 提供 type-safe 的 OpenSSH wrapper：
- 仍然 spawn ssh，保留 100% 的 SSH config 相容性
- 但提供 Rust-native 的 Session/Channel 抽象
- 內建 ControlMaster 管理
- 如果主要痛點是 ControlMaster 管理的複雜性，這可能是更好的選擇

### 7.3 方案 E（Feature gate）

```toml
[features]
default = ["system-ssh"]
system-ssh = []        # 現有 spawn ssh/scp 行為
embedded-ssh = ["russh", "russh-keys", "russh-sftp", "ssh2-config"]
```

同時維護兩個傳輸後端，由 compile-time feature flag 切換。好處是不破壞現有用戶，壞處是維護成本翻倍。

---

## 八、邊界情況清單

### 8.1 認證邊界

- [ ] SSH Agent 不可用時（SSH_AUTH_SOCK 不存在）
- [ ] SSH Agent 裡有多把 key，第一把不對
- [ ] IdentityFile 路徑含 `~`（需要展開）
- [ ] IdentityFile 含 `%h`/`%p` 等 token（OpenSSH 支援）
- [ ] 加密私鑰（需要 passphrase）
- [ ] FIDO key（ed25519-sk）
- [ ] Certificate-based auth（`-cert.pub` 檔）
- [ ] `IdentitiesOnly yes`（只嘗試指定的 key）

### 8.2 連線邊界

- [ ] IPv6 literal address（`[::1]`）
- [ ] 非標準 port
- [ ] 連線途中被 RST
- [ ] DNS 解析失敗
- [ ] TCP keepalive timeout
- [ ] SSH banner 過長
- [ ] 伺服器端限制 channel 數量
- [ ] 伺服器端禁用 SFTP subsystem

### 8.3 SFTP 邊界

- [ ] 符號連結（跟隨 vs 不跟隨）
- [ ] 檔案權限保持
- [ ] 大檔案（>1GB）的記憶體使用
- [ ] 遠端磁碟滿
- [ ] 路徑含空格或特殊字元
- [ ] Windows 遠端的路徑分隔符（`\` vs `/`）
- [ ] SFTP 不支援 `~` 展開（需要先解析 home dir）

### 8.4 SSH Config 邊界

- [ ] `Include` directive 引入其他檔案
- [ ] `Match` conditional blocks
- [ ] 多個 `Host` pattern 在同一行（`Host a b c`）
- [ ] `CanonicalizeHostname`
- [ ] 環境變數展開
- [ ] `%h`, `%p`, `%r` 等 token 替換

### 8.5 並行邊界

- [ ] 同一 Session 上同時開 10+ channel
- [ ] SFTP channel 與 exec channel 並行
- [ ] Session 在某個 channel 操作中斷線
- [ ] Handle 被多個 tokio task 同時使用時的 ordering

---

## 九、建議遷移策略

### 如果決定遷移

**Phase 0：前置驗證（1-2 天）**
- 用 russh 寫一個獨立的 PoC：連線到你的 homelab 一台機器
- 驗證：agent auth、known_hosts check、channel exec、SFTP upload/download
- 測量效能對比（spawn scp vs russh-sftp 傳同一個檔案）
- **如果 PoC 發現 blocker（如 FIDO key），在此止步**

**Phase 1：傳輸層抽象（3-5 天）**
- 定義 `SshTransport` trait：
  ```rust
  #[async_trait]
  pub trait SshTransport {
      async fn exec(&self, host: &str, cmd: &str, timeout: u64) -> Result<RemoteOutput>;
      async fn upload(&self, host: &str, local: &Path, remote: &str, timeout: u64) -> Result<()>;
      async fn download(&self, host: &str, remote: &str, local: &Path, timeout: u64) -> Result<()>;
      async fn sftp_stat(&self, host: &str, path: &str) -> Result<Option<FileStat>>;
  }
  ```
- 先為現有 spawn ssh/scp 實作此 trait
- 所有 command 改用 trait

**Phase 2：russh 實作（5-7 天）**
- 實作 `RusshTransport`
- 包含：Session pool、auth chain、known_hosts、SFTP
- 用 feature flag 切換

**Phase 3：整合測試（2-3 天）**
- 在所有 homelab host 上測試
- 對比 spawn ssh 與 russh 的行為差異
- 效能測試

**Phase 4：切換（1 天）**
- 將 default feature 切為 `embedded-ssh`
- 保留 `system-ssh` 作為 fallback

### 如果決定不遷移

**建議改進方案**：
1. 引入 `openssh` crate 取代手工 `Command::new("ssh")` 組裝
2. 在 `executor.rs` 上加一層 `trait SshExecutor`，為未來遷移預留抽象
3. 改善 error handling：解析 ssh stderr 提供更好的錯誤訊息

---

## 十、結論

### 核心判斷

**遷移的技術可行性：✅ 可行**——russh 足夠成熟，且 ssync 的 SSH 使用模式（exec + SFTP）在 russh 支援範圍內。

**遷移的必要性：⚠️ 低到中等**——ssync 當前架構運作良好，主要痛點（ControlMaster 管理、Windows 相容性）有更輕量的解法。

**遷移的 ROI：⚠️ 不確定**——需要 PoC 驗證後才能確定收益是否值得 ~3,000 行的改動和長期維護成本。

### 決策建議

1. **先做 Phase 0 的 PoC**——這是零風險的驗證步驟
2. **在 PoC 中特別驗證**：
   - 你 homelab 所有主機的認證方式（agent? key? FIDO?）
   - 是否有任何主機需要 ProxyJump
   - SFTP 在你的 host 上是否都啟用
3. **PoC 通過後**，評估 Phase 1 的 trait 抽象是否值得——即使最終不遷移 russh，這個抽象也能改善程式碼品質
4. **如果 PoC 發現 blocker**，考慮方案 D（openssh crate）或維持現狀
