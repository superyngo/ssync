# ssync — 專案大綱

> 以 `~/.ssh/config` 為基礎的個人跨平台遠端管理工具

---

## 目錄

1. [功能說明](#1-功能說明)
2. [技術棧](#2-技術棧)
3. [專案架構](#3-專案架構)
4. [安裝後資料夾與檔案結構](#4-安裝後資料夾與檔案結構)
5. [設定檔規範](#5-設定檔規範)
6. [子命令及參數](#6-子命令及參數)
7. [DB Schema](#7-db-schema)

---

## 1. 功能說明

### 定位

ssync 是一個單一 binary 的命令列工具，無需在遠端安裝任何 client，以使用者既有的 `~/.ssh/config` 作為 host 來源，提供檔案同步、系統狀態監控與遠端執行等功能。

### 核心特性

- **無 client**：遠端只需開放 SSH，不安裝任何 agent
- **借用 ~/.ssh/config**：自動繼承 ProxyJump、IdentityFile 等既有設定
- **跨平台**：支援遠端 sh / PowerShell / cmd 三種 shell 環境
- **並行優先**：預設並行連線多台 host，`--serial` 改為順序執行
- **單次運行**：不常駐背景，適合手動呼叫或系統排程（cron / Task Scheduler）

### 子功能概覽

| 功能         | 說明                                                                      |
| ------------ | ------------------------------------------------------------------------- |
| **init**     | 從 `~/.ssh/config` 匯入 hosts，自動偵測遠端 shell 類型，建立或更新 config |
| **check**    | 並行收集指定 host/group 的系統快照，儲存至 state DB                       |
| **checkout** | 從 state DB 讀取歷史資料，產生報表（TUI / HTML / JSON）                   |
| **sync**     | 依最新版本雙向同步檔案與資料夾，支援衝突策略                              |
| **run**      | 對遠端 host 執行指令字串                                                  |
| **exec**     | 將本機腳本上傳並在遠端執行，shell 不相容自動跳過                          |
| **log**      | 查看操作紀錄                                                              |

---

## 2. 技術棧

### 語言與執行環境

- **Rust** (edition 2021)：編譯成單一靜態 binary，無 runtime 依賴

### 核心 Crate

| Crate                  | 用途                           |
| ---------------------- | ------------------------------ |
| `clap` v4 (derive)     | CLI 子命令與參數解析           |
| `tokio` v1 (full)      | async 並行執行多 host 任務     |
| `serde` + `toml`       | config.toml 序列化/反序列化    |
| `rusqlite` (bundled)   | state DB，bundled 避免系統依賴 |
| `sha2`                 | 檔案 hash（同步比對用）        |
| `chrono`               | 時間戳記、資料保存期限計算     |
| `anyhow` + `thiserror` | 錯誤處理                       |
| `indicatif`            | 並行進度列與即時輸出           |
| `ratatui`              | checkout TUI 報表              |
| `comrak` / 直接 HTML   | checkout HTML 報表產生         |

### SSH 策略：Shell Out（優先選項）

直接呼叫系統 `ssh` / `scp` / `rsync`，自動繼承使用者所有 ssh config 設定（ProxyJump、known_hosts、ssh-agent）。

```
std::process::Command → ssh / scp / rsync
```

優點：零額外 SSH 依賴，cross-compile 無問題，繼承複雜 ProxyJump 拓樸。
缺點：錯誤需自行 parse stdout/stderr。

### Cross-compile 目標（建議支援）

| Target                       | 說明                    |
| ---------------------------- | ----------------------- |
| `x86_64-unknown-linux-gnu`   | Linux x86_64            |
| `x86_64-unknown-linux-musl`  | Linux x86_64 (musl)     |
| `i686-unknown-linux-gnu`     | Linux x86 32-bit        |
| `i686-unknown-linux-musl`    | Linux x86 32-bit (musl) |
| `aarch64-unknown-linux-gnu`  | Linux ARM64             |
| `aarch64-unknown-linux-musl` | Linux ARM64 (musl)      |
| `x86_64-apple-darwin`        | macOS Intel             |
| `aarch64-apple-darwin`       | macOS Apple Silicon     |
| `x86_64-pc-windows-msvc`     | Windows x86_64          |
| `i686-pc-windows-msvc`       | Windows x86 32-bit      |

---

## 3. 專案架構

```
ssync/
├── Cargo.toml
├── Cargo.lock
├── README.md
│
└── src/
    ├── main.rs                  # 程式入口，CLI dispatch
    │
    ├── cli.rs                   # clap 結構定義（所有子命令與參數）
    │
    ├── config/
    │   ├── mod.rs
    │   ├── app.rs               # 讀寫 ~/.config/ssync/config.toml
    │   ├── schema.rs            # serde 結構體（AppConfig, HostEntry, SyncGroup…）
    │   └── ssh_config.rs        # 解析 ~/.ssh/config，取 Host 清單與連線參數
    │
    ├── state/
    │   ├── mod.rs
    │   ├── db.rs                # SQLite 連線、migration、CRUD
    │   └── retention.rs         # 依 retention_days 清理舊資料
    │
    ├── host/
    │   ├── mod.rs
    │   ├── executor.rs          # SSH 指令執行抽象（run_remote / upload / download）
    │   ├── shell.rs             # Shell enum（Sh / PowerShell / Cmd）及指令模板
    │   └── filter.rs            # --group / --host / --all 過濾邏輯
    │
    ├── output/
    │   ├── mod.rs
    │   ├── printer.rs           # 並行即時輸出（host prefix、顏色）
    │   └── summary.rs           # 執行結束後 summary block
    │
    ├── commands/
    │   ├── mod.rs
    │   ├── init.rs              # init 子命令
    │   ├── check.rs             # check 子命令
    │   ├── checkout.rs          # checkout 子命令（TUI / HTML / JSON）
    │   ├── sync.rs              # sync 子命令
    │   ├── run.rs               # run 子命令
    │   ├── exec.rs              # exec 子命令
    │   └── log.rs               # log 子命令
    │
    └── metrics/
        ├── mod.rs
        ├── collector.rs         # 依 shell 類型選擇對應探測指令
        ├── parser.rs            # 解析各平台指令輸出為統一結構
        └── probes/
            ├── sh.rs            # Linux/macOS 探測指令集
            ├── powershell.rs    # PowerShell 探測指令集
            └── cmd.rs           # Windows CMD 探測指令集
```

---

## 4. 安裝後資料夾與檔案結構

```
~/.config/ssync/
└── config.toml              # 主設定檔（init 建立，使用者手動維護）

~/.local/state/ssync/
├── ssync.db                 # SQLite state DB（check 快照、sync 狀態、操作 log）
└── ssync.log                # 文字 log（操作摘要，供 log 子命令讀取）
```

ssync 本體為單一 binary，建議安裝至 `~/.local/bin/ssync` 或 `/usr/local/bin/ssync`。

---

## 5. 設定檔規範

### 完整範例：`~/.config/ssync/config.toml`

```toml
# ── 全域設定 ────────────────────────────────────────────
[settings]
default_timeout    = 30          # 連線逾時秒數
data_retention_days = 90         # check 快照保存天數
conflict_strategy  = "newest"   # sync 衝突策略：newest | skip
propagate_deletes  = false       # sync 是否傳播刪除

# ── Host 定義 ────────────────────────────────────────────
# init 自動從 ~/.ssh/config 匯入，shell 欄位由自動偵測填入

[[host]]
name     = "home-linux"
ssh_host = "home-linux"          # 對應 ~/.ssh/config 的 Host 名稱
shell    = "sh"                  # sh | powershell | cmd
groups   = ["personal", "all"]

[[host]]
name     = "work-win"
ssh_host = "work-windows"
shell    = "powershell"
groups   = ["work", "all"]

[[host]]
name     = "vps"
ssh_host = "my-vps"
shell    = "sh"
groups   = ["personal", "all"]

# ── check 監控項目設定 ────────────────────────────────────
[check]
# 全域啟用的監控項目
enabled = [
  "online",       # 是否在線（離線則顯示最後上線時間）
  "system_info",  # OS、hostname、kernel
  "cpu_arch",     # CPU 架構
  "memory",       # 記憶體用量
  "swap",         # Swap 用量
  "disk",         # 磁碟用量（根目錄）
  "cpu_load",     # CPU 負載
  "network",      # IP、實時網路用量
  "battery",      # 電池狀態（無電池則跳過）
]

# 指定路徑的容量監控（可多個）
[[check.path]]
path  = "~/projects"
label = "Projects"

[[check.path]]
path  = "D:/Work"
label = "Work Drive"

# ── sync 群組設定 ────────────────────────────────────────
[[sync.group]]
name  = "dotfiles"
hosts = ["home-linux", "vps"]    # 參與同步的 host 名稱

  [[sync.group.file]]
  path = "~/.vimrc"
  mode = "0644"                  # 可選，同步後套用的檔案權限

  [[sync.group.file]]
  path      = "~/.config/nvim"
  recursive = true

[[sync.group]]
name  = "notes"
hosts = ["home-linux", "vps", "work-win"]

  [[sync.group.file]]
  path              = "~/notes"
  recursive         = true
  propagate_deletes = true       # 覆蓋全域設定
```

### 欄位說明

#### `[settings]`

| 欄位                  | 型別    | 預設     | 說明                         |
| --------------------- | ------- | -------- | ---------------------------- |
| `default_timeout`     | integer | 30       | SSH 連線逾時（秒）           |
| `data_retention_days` | integer | 90       | check 快照保存天數，0 = 永久 |
| `conflict_strategy`   | string  | "newest" | `newest` \| `skip`           |
| `propagate_deletes`   | bool    | false    | sync 是否傳播刪除            |

#### `[[host]]`

| 欄位       | 型別     | 必填 | 說明                              |
| ---------- | -------- | ---- | --------------------------------- |
| `name`     | string   | ✓    | 工具內部識別名稱                  |
| `ssh_host` | string   | ✓    | 對應 `~/.ssh/config` 的 `Host` 值 |
| `shell`    | string   | ✓    | `sh` \| `powershell` \| `cmd`     |
| `groups`   | [string] |      | 所屬群組，可多個                  |

#### `[check]`

| 欄位      | 型別     | 說明               |
| --------- | -------- | ------------------ |
| `enabled` | [string] | 啟用的監控項目清單 |

#### `[[check.path]]`

| 欄位    | 型別   | 說明                      |
| ------- | ------ | ------------------------- |
| `path`  | string | 遠端路徑（支援 `~` 展開） |
| `label` | string | 顯示標籤                  |

#### `[[sync.group]]`

| 欄位    | 型別     | 說明                      |
| ------- | -------- | ------------------------- |
| `name`  | string   | 群組名稱                  |
| `hosts` | [string] | 參與同步的 host name 清單 |

#### `[[sync.group.file]]`

| 欄位                | 型別   | 預設     | 說明                                |
| ------------------- | ------ | -------- | ----------------------------------- |
| `path`              | string |          | 同步路徑                            |
| `recursive`         | bool   | false    | 資料夾遞迴同步                      |
| `mode`              | string |          | 同步後套用的 Unix 權限，如 `"0644"` |
| `propagate_deletes` | bool   | 繼承全域 | 是否傳播刪除                        |

---

## 6. 子命令及參數

### 通用參數（所有子命令共用）

```
-v, --version             顯示版本資訊
-V, --verbose             顯示詳細執行資訊
-g, --group <name,...>    指定 group（可多個，逗號分隔）
-h, --host  <name,...>    指定 host（可多個，與 --group 取交集）
    --all                 明確指定全部 host（避免裸執行意外）
    --serial              順序執行（預設：並行）
    --timeout <secs>      連線逾時（覆蓋 config 預設值）
```

---

### `ssync init`

從 `~/.ssh/config` 匯入 host 清單，SSH 連線偵測遠端 shell 類型，建立或更新 `config.toml`。

```
ssync init [OPTIONS]

Options:
    --update     重新偵測已有 host 的 shell 類型
    --dry-run    僅顯示會匯入的內容，不寫入
```

**流程：**

1. 讀取 `~/.ssh/config`，列出所有 `Host`（排除萬用字元）
2. 並行 SSH 連線，執行 shell 偵測指令（`uname` / `$PSVersionTable` / `ver`）
3. 顯示偵測結果，請使用者確認
4. 寫入 `~/.config/ssync/config.toml`

---

### `ssync check`

並行收集指定 host 的系統快照，儲存至 state DB。

```
ssync check [OPTIONS]
            [-g <group>] [-h <host>] [--all]
            [--serial] [--timeout <secs>]
```

**收集項目（依 config `[check].enabled` 決定）：**

| 項目          | sh 指令來源                        | PowerShell 來源                                     | CMD 來源                         |
| ------------- | ---------------------------------- | --------------------------------------------------- | -------------------------------- |
| online        | `echo ok`                          | `echo ok`                                           | `echo ok`                        |
| system_info   | `uname -a`, `hostname`             | `Get-ComputerInfo`                                  | `systeminfo`                     |
| cpu_arch      | `uname -m`                         | `$env:PROCESSOR_ARCHITECTURE`                       | `wmic cpu get AddressWidth`      |
| memory        | `free -b`                          | `Get-CimInstance Win32_OperatingSystem`             | `wmic OS get FreePhysicalMemory` |
| swap          | `free -b`                          | `Get-CimInstance Win32_PageFileUsage`               | `wmic pagefile`                  |
| disk          | `df -B1`                           | `Get-PSDrive`                                       | `wmic logicaldisk`               |
| cpu_load      | `/proc/loadavg`                    | `Get-Counter '\Processor(_Total)\% Processor Time'` | `wmic cpu get LoadPercentage`    |
| network       | `ip -j addr` + `cat /proc/net/dev` | `Get-NetIPAddress` + `Get-NetAdapterStatistics`     | `ipconfig` + `netstat`           |
| battery       | `/sys/class/power_supply`          | `Get-WmiObject Win32_Battery`                       | `wmic path Win32_Battery`        |
| path capacity | `du -sb <path>`                    | `(Get-Item <path>).length` / `du`                   | `dir /s`                         |

**輸出格式（即時）：**

```
[home-linux]  ✓ collected (12 metrics, 0.8s)
[work-win  ]  ✓ collected (10 metrics, battery: skipped, 1.2s)
[vps       ]  ✗ connection timeout

── Summary ──────────────────────────────
  2 succeeded  1 failed
  Errors:
    vps: connection timeout
```

---

### `ssync checkout`

從 state DB 讀取歷史資料，產生報表。

```
ssync checkout [OPTIONS]
               [-g <group>] [-h <host>] [--all]

Options:
    --format <fmt>      tui | html | json  （預設：tui）
    --history           顯示趨勢（預設：顯示最新一筆快照）
    --since  <datetime> 趨勢歷史資料起點（如 "2025-01-01" 或 "7d"）
    --out/-o <path>     輸出檔案路徑（html / json 必填）
```

**TUI 模式：**

- 表格顯示各 host 最新狀態
- 橫向列為 host，縱向列為 metric
- 歷史趨勢以 ASCII 折線圖呈現（記憶體、CPU 負載等）
- 離線 host 顯示最後上線時間

**HTML 模式：**

- 靜態 HTML，內嵌 CSS/JS
- 使用 Chart.js 繪製趨勢圖
- 可開啟於瀏覽器，無需 server

**JSON 模式：**

- 輸出原始結構化資料，供外部工具處理

---

### `ssync sync`

依最新版本雙向同步檔案或資料夾。

```
ssync sync [OPTIONS]
           [-g <group>] [-h <host>] [--all]
           [--serial] [--dry-run] [--timeout <secs>]
```

**兩階段執行：**

1. 並行收集所有 host 的檔案 mtime + hash → 比對決定 source
2. 並行傳輸（以 scp / rsync shell out 執行）

**衝突策略（依 config `conflict_strategy`）：**

| 策略     | 說明                                   |
| -------- | -------------------------------------- |
| `newest` | 以 mtime 最新的版本為準，覆蓋其他 host |
| `skip`   | 有衝突時跳過，僅在 log 中記錄          |

**資料夾處理：**

- `recursive = true`：逐檔案比對，個別同步
- `propagate_deletes = false`（預設）：不傳播刪除，v1 安全起點

---

### `ssync run`

對遠端 host 執行指令字串。

```
ssync run <COMMAND> [OPTIONS]
          [-g <group>] [-h <host>] [--all]
          [--serial] [--timeout <secs>]
          [-s, --sudo]
          [-y, --yes]

Arguments:
    <COMMAND>    要執行的指令字串

Options:
    -s, --sudo    以 sudo 執行（需遠端設定 NOPASSWD）
    -y, --yes     自動回應互動式 yes/no（僅 --serial 模式有效）
```

**範例：**

```bash
ssync run "df -h" --all
ssync run "apt-get update" --group servers --sudo
ssync run "Get-Service" --host work-win
```

**互動式處理：**

- 預設：非互動，遠端若等待輸入則 timeout 報錯
- `--serial --yes`：自動回應 yes/no
- `--serial`（無 `--yes`）：透傳本機 stdin，讓使用者直接互動

---

### `ssync exec`

將本機腳本上傳並在遠端執行。

```
ssync exec <SCRIPT> [OPTIONS]
           [-g <group>] [-h <host>] [--all]
           [--serial] [--dry-run] [--timeout <secs>]
           [-s, --sudo]
           [-y, --yes]
           [--keep]

Arguments:
    <SCRIPT>    本機腳本路徑

Options:
    -s, --sudo    以 sudo 執行（需遠端設定 NOPASSWD）
    -y, --yes     自動回應 yes/no（僅 --serial 模式有效）
    --keep        執行後保留遠端暫存腳本（預設執行完自動刪除）
```

**Shell 相容性對應：**

| 副檔名        | 相容 shell   | 不相容時                                      |
| ------------- | ------------ | --------------------------------------------- |
| `.sh`         | `sh`         | 跳過，summary 標記 `skipped (shell mismatch)` |
| `.ps1`        | `powershell` | 跳過                                          |
| `.bat` `.cmd` | `cmd`        | 跳過                                          |

**執行流程：**

1. 判斷腳本副檔名
2. 過濾相容的 host（不相容自動跳過）
3. 上傳腳本至遠端暫存路徑（`/tmp/` 或 `%TEMP%`）
4. 執行
5. 刪除暫存腳本（除非 `--keep`）

---

### `ssync log`

查看操作紀錄。

```
ssync log [OPTIONS]

Options:
    --last <n>          顯示最後 n 筆（預設：20）
    --since <datetime>  顯示指定時間後的記錄
    -h, --host <name>   過濾特定 host
    --action <type>     過濾操作類型：sync | run | exec | check
    --errors            只顯示錯誤記錄
```

---

## 7. DB Schema

### 資料庫位置

`~/.local/state/ssync/ssync.db`（SQLite，rusqlite bundled）

---

### `check_snapshots` — 系統監控快照

```sql
CREATE TABLE check_snapshots (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    host         TEXT    NOT NULL,   -- host.name
    collected_at INTEGER NOT NULL,   -- Unix timestamp（秒）
    online       INTEGER NOT NULL,   -- 1 = online, 0 = offline
    raw_json     TEXT    NOT NULL    -- 當次收集到的完整 JSON payload
);

CREATE INDEX idx_check_snapshots_host_time
    ON check_snapshots (host, collected_at DESC);
```

**raw_json 結構範例：**

```json
{
  "system_info": {
    "os": "Ubuntu 22.04",
    "hostname": "home-linux",
    "kernel": "5.15.0"
  },
  "cpu_arch": "x86_64",
  "memory": { "total_bytes": 16777216000, "used_bytes": 8388608000 },
  "swap": { "total_bytes": 4294967296, "used_bytes": 1073741824 },
  "disk": [
    { "mount": "/", "total_bytes": 107374182400, "used_bytes": 53687091200 }
  ],
  "cpu_load": { "load1": 0.52, "load5": 0.38, "load15": 0.21 },
  "network": {
    "interfaces": [
      { "name": "eth0", "ipv4": "192.168.1.10", "ipv6": "fe80::1" }
    ],
    "rx_bytes_per_sec": 12480,
    "tx_bytes_per_sec": 4096
  },
  "battery": { "present": true, "percent": 87, "status": "Discharging" },
  "paths": [
    { "label": "Projects", "path": "~/projects", "size_bytes": 2147483648 }
  ]
}
```

---

### `host_last_seen` — Host 最後上線記錄

```sql
CREATE TABLE host_last_seen (
    host        TEXT    PRIMARY KEY,
    last_seen   INTEGER NOT NULL,    -- Unix timestamp
    last_online INTEGER NOT NULL     -- 最後成功連線的 Unix timestamp
);
```

---

### `sync_state` — 檔案同步狀態

```sql
CREATE TABLE sync_state (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    sync_group   TEXT    NOT NULL,   -- sync.group.name
    host         TEXT    NOT NULL,   -- host.name（"local" 代表本機）
    path         TEXT    NOT NULL,   -- 檔案相對路徑
    mtime        INTEGER NOT NULL,   -- Unix timestamp
    size_bytes   INTEGER NOT NULL,
    sha256       TEXT    NOT NULL,
    synced_at    INTEGER NOT NULL,   -- 最後同步時間
    UNIQUE (sync_group, host, path)
);

CREATE INDEX idx_sync_state_group
    ON sync_state (sync_group, host);
```

---

### `operation_log` — 操作紀錄

```sql
CREATE TABLE operation_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp  INTEGER NOT NULL,    -- Unix timestamp
    command    TEXT    NOT NULL,    -- "sync" | "run" | "exec" | "check"
    host       TEXT    NOT NULL,
    action     TEXT    NOT NULL,    -- 操作描述，如 "copy ~/.vimrc"
    status     TEXT    NOT NULL,    -- "ok" | "error" | "skipped"
    duration_ms INTEGER,            -- 執行時間（毫秒）
    note       TEXT                 -- 錯誤訊息或補充資訊
);

CREATE INDEX idx_operation_log_time
    ON operation_log (timestamp DESC);

CREATE INDEX idx_operation_log_host
    ON operation_log (host, timestamp DESC);
```

---

### Migration 策略

- DB 版本記錄在 `PRAGMA user_version`
- 程式啟動時自動執行 migration（若版本低於當前）
- Migration 腳本內嵌於 binary（`include_str!` 載入 `.sql` 檔）

```sql
-- 版本查詢
PRAGMA user_version;

-- 版本設定（migration 後執行）
PRAGMA user_version = 2;
```

---

### 資料清理（Retention）

`check` 執行結束後，依 `settings.data_retention_days` 自動清理：

```sql
DELETE FROM check_snapshots
WHERE collected_at < (strftime('%s', 'now') - (? * 86400));
-- ? = data_retention_days
```

`operation_log` 保留策略相同，或可設個別保存期限（未來擴充）。

---

_文件版本：v0.1.0-draft_
