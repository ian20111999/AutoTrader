# ADR-003：把 K 線層級的訂單流欄位（成交筆數、主動買盤量）接進 `Bar`

- **狀態**：Proposed（等實作前的審查；本文件不含任何實作程式碼）
- **日期**：2026-10-02
- **決策者**：架構研發部
- **動機**：CEO 要求「能設定更細緻的高頻策略，用真實資料回測」
- **影響的 crate**：`at-core`（`Bar`／`kline_csv`／`bar_store`／`strategy_dsl`／`strategy`）、
  `at-market-stream`、`at-binance-client`、`app/src-tauri`
- **不影響**：`at-engine`、`at-risk-control`、`at-portfolio-risk`、`at-session-store`、
  `at-secret-store`、`at-account-sync`、四個內建策略（均線交叉／布林／唐奇安／RSI）
- **範圍邊界**：只做「K 線層級」的訂單流。**不含** tick／逐筆成交資料，
  **不含** 積木 UI 怎麼呈現這些指標（下一個 Phase），**不含** `Interval::S1`（另一條平行工作）。

> 編號說明：`docs/architecture/` 目前有兩份都叫 ADR-001（session-registry、strategy-dsl）
> 與一份 ADR-002。本文件取 ADR-003，不回頭重編既有文件（不在本次範圍）。

---

## 1. 背景與問題

### 1.1 現狀盤點（事實，附檔案行號）

Binance 官方歷史 K 線 CSV（`data.binance.vision`）每行 12 欄，
`crates/core/src/kline_csv.rs:4-13` 的 doc comment 自己寫著：

```text
open_time(ms), open, high, low, close, volume, close_time(ms),
quote_asset_volume, number_of_trades,
taker_buy_base_asset_volume, taker_buy_quote_asset_volume, ignore
```

> 「這裡只用前 6 欄建 `Bar`；其餘欄位目前 `Bar` 沒地方放，先不管
> （成交筆數、吃單量之後真的要用時再加）。」

**現在就是「之後」。** 被丟棄的三個欄位是真實的訂單流訊號：

| Binance 欄位 | CSV 索引 | WebSocket `k` 欄位 | 語意 |
|---|---|---|---|
| `number_of_trades` | `[8]` | `n` | 這根 K 線內的成交筆數 |
| `taker_buy_base_asset_volume` | `[9]` | `V` | 主動買方（taker）成交量，基礎幣計 |
| `taker_buy_quote_asset_volume` | `[10]` | `Q` | 同上，報價幣計 |
| `quote_asset_volume` | `[7]` | `q` | 總成交額，報價幣計 |

「主動買盤佔比」= 主動買方成交量 ÷ 總成交量，是標準的買賣壓力失衡指標。
**不需要額外抓 tick 資料** —— 這些數字本來就在每一根 K 線裡，而且本機已下載的
原始 Binance CSV 裡也有，只是解析時被丟掉。

三個丟棄點：

| 位置 | 現況 |
|---|---|
| `crates/core/src/kline_csv.rs:75-82` | `parse_line()` 只取 `fields[0..6]` |
| `crates/market-stream/src/lib.rs:362-394` | `RawKline` 只宣告 `t/i/o/h/l/c/v/x`，`n`／`V`／`q`／`Q` 連欄位都沒宣告 |
| `crates/binance-client/src/market_data.rs:103-134` | REST `/api/v3/klines`（暖機來源）只讀 `row[0..6]` |

### 1.2 為什麼回溯相容是這份設計的核心問題

`Bar`（`crates/core/src/bar.rs:96-105`）是全引擎最基礎的型別：

- **26 處**程式碼用 `Bar { open_time, open, high, low, close, volume }` 字面建構（實測，見 §7.3）。
- **本機已存在的歷史檔案是 6 欄格式**：`crates/core/src/bar_store.rs:1-14` 的自有格式只存
  `Bar` 真正有的 6 個欄位。使用者機器上 1.7 下載器存下來的檔案全是這個格式。
- **repo 裡就有一份**：`crates/downloader/tests/fixtures/BTCUSDT/1d/BTCUSDT-1d-2024-01.csv`
  是 6 欄的本機格式檔，被 `tests/integration_sma_cross_validation.rs:213` 用
  `read_bars_file()` 讀進來，而且那支測試用 golden 權益曲線逐點比對。
  **這份檔案不能重新產生就過不了測試** —— 它是「舊格式必須繼續可讀」最硬的證據。

### 1.3 這份 ADR 要回答的六個問題

1. `Bar` 直接加欄位，還是另外包一層平行結構？
2. 本機既有的 6 欄檔案怎麼處理？
3. 新欄位用 `Option` 還是強制有值？
4. 本機存檔格式怎麼升版？
5. DSL 引擎側怎麼讀到新欄位？
6. 改動牽動哪些檔案、有多少處要補欄位？

---

## 2. 決策摘要

| # | 決策 |
|---|---|
| D1 | **方案 A：直接在 `Bar` 加欄位**，但只加**一個**：`order_flow: Option<OrderFlow>`。不做平行陣列。 |
| D2 | `OrderFlow` 存**交易所原始觀測值**（`trades`、`taker_buy_volume`），**不存**預先算好的比例。 |
| D3 | 用 `Option<OrderFlow>` 表示「這個來源有沒有提供訂單流」；「這根真的沒成交」用 `Some(OrderFlow { trades: 0, taker_buy_volume: 0.0 })` 表示。兩者在型別上就分得開。 |
| D4 | 本機格式**用欄位數判版**（6 欄＝舊、8 欄＝新），不加版本號行。舊檔讀進來 `order_flow: None`，**不要求重新下載**。同一個檔案內不可混欄數。 |
| D5 | DSL 加兩個 `PriceField` variant（`trades`、`taker_buy_ratio`），**不新增指標、不改任何 wire format tag**。比例在讀取時才除，`volume == 0` → `None`。 |
| D6 | `Strategy` trait 加一個預設回 `false` 的 `needs_order_flow()`，載入端據此在資料沒有訂單流時**硬錯誤**，防止「靜默零交易回測」。 |

**一句話總結**：`Bar` 加一個 `Option` 子結構裝原始數字，比例留到 DSL 讀取時才算，
本機檔案用欄位數自己說自己是哪個版本，然後用一個 trait 方法把
「策略要訂單流但資料沒有」從靜默錯誤變成大聲錯誤。

**新增依賴：0 個。新增 crate：0 個。新增型別：1 個。**

---

## 3. 問題一：直接加欄位，還是包一層？

### 方案 A：直接在 `Bar` 上加欄位

```rust
pub struct Bar {
    pub open_time: i64,
    pub open: Fixed, pub high: Fixed, pub low: Fixed, pub close: Fixed,
    pub volume: f64,
    pub order_flow: Option<OrderFlow>,   // ← 新增
}
```

- 策略讀取最直接：`bar.order_flow`，`Strategy::on_bar(&mut self, bar: &Bar)` 一個字都不用改。
- 代價：26 處字面建構要補欄位。

### 方案 B：平行結構（`Vec<Bar>` + `Vec<OrderFlow>`）

`Bar` 不動，訂單流另外一條陣列，策略要用時另外傳。

- 改動範圍看起來小（`Bar` 零改動）。
- 但要付出的代價是**介面與不變量**：
  - `on_bar(&mut self, bar: &Bar)` 得變成 `on_bar(&mut self, bar: &Bar, flow: Option<&OrderFlow>)`
    或新增第二個 trait 方法 —— 四個內建策略、`LeveragedStrategy`、`CustomStrategy`、
    回測迴圈、`WarmupBars::replay`、`PaperEngine`、測試網 `Trader` 全部要改簽名。
    **這比 26 行 `order_flow: None` 貴得多。**
  - 「`Vec<OrderFlow>` 必須永遠和 `Vec<Bar>` 逐根對齊」是一個沒有型別保護的不變量。
    `find_gaps` 回報的缺口、`bars[i]` 的切片、warmup 回放的 `&bars[..n]`、
    `backtest.rs` 裡每一個 `enumerate()` —— 每一處都是一個對不齊的機會，
    而對不齊的症狀是「訊號偏移一根」，**不會 panic、不會報錯，只會算出一份錯的回測**。
  - 本機存檔要變成兩個檔案（或一個檔案兩段），`write_bars_file` 的原子性沒了。

### 方案 C：`Bar` 加 trait，訂單流走另一個型別參數

為一個 struct 加泛型／trait 抽象層。**直接否決**：單一實作的 interface，
`Bar` 會從一個 48 bytes 的 plain data 變成需要讀懂泛型才能用的型別，
而唯一的好處是「`Bar` 的欄位沒變」，那不是好處。

### 比較

| 項目 | 方案 A（加欄位） | 方案 B（平行陣列） | 方案 C（抽象層） |
|---|---|---|---|
| `Bar` 的 diff | +1 欄位 | 0 | 0 |
| 字面建構要補的地方 | 26 處（一行一處） | 0 | 0 |
| `Strategy` trait 簽名 | **不動** | 必改，所有實作連帶 | 必改 |
| 新的不變量 | 無（`Option` 自帶語意） | 「兩條陣列永遠對齊」，無型別保護 | 無 |
| 對不齊的失敗模式 | 不存在 | **靜默算錯訊號** | 不存在 |
| 存檔格式 | 一個檔案多 2 欄 | 兩個檔案要同步 | 一個檔案 |
| 新手讀得懂 | 是 | 要先理解兩條陣列的關係 | 否 |

### 決定：方案 A

26 行 `order_flow: None`（而且大多落在各檔案唯一的測試輔助函式裡，實際約 20 個編輯點）
是一次性的機械成本，編譯器會把每一處指出來，漏一處就編不過。
方案 B 省下的是這 26 行，換來的是一個永久的、沒有編譯器保護、
失敗時不會報錯只會算錯的對齊不變量 —— 在一個決定要不要下單的引擎裡，這個交易不划算。

**但決定只加一個欄位，不是兩個平行的 `Option`。** 理由見 §5。

---

## 4. 問題二：存原始值，還是存算好的比例？

兩種切法：

```rust
// 切法 1：存算好的比例
pub struct OrderFlow { trades: u64, taker_buy_ratio: Option<Fixed> }

// 切法 2：存原始觀測值（選定）
pub struct OrderFlow { trades: u64, taker_buy_volume: f64 }
```

| 項目 | 切法 1（存比例） | 切法 2（存原始值） |
|---|---|---|
| 存檔 round-trip | 存的是衍生值，來源數字永久遺失 | 和交易所 CSV 逐字對應，可重算任何衍生值 |
| `validate()` 能檢查什麼 | 只能檢查 `0 ≤ r ≤ 1`（弱） | 可檢查 `0 ≤ taker_buy_volume ≤ volume`（**真的能抓到壞檔**） |
| CVD（累積成交量差）可不可能 | 不行（`volume` 是 f64，比例已截尾，差額算不準） | 可以：`2 × taker_buy_volume − volume` 精確 |
| 「這根沒成交」怎麼表示 | `taker_buy_ratio: None` —— 和「來源沒提供」撞語意 | `trades: 0, taker_buy_volume: 0.0` —— 和「來源沒提供」是不同型別狀態 |
| 除法發生在哪 | 解析時（CSV、WebSocket、REST **三個地方各一次**） | 讀取時（DSL `price_field` **一個地方**） |

### 決定：切法 2（存原始值）

決定性的是最後兩列：

1. **存原始值讓「沒成交」和「沒資料」在型別上分開**（這正是問題三要解決的事，見 §5）。
   存比例的話，`None` 同時是「舊檔案沒這個欄位」和「分母為 0 算不出來」，分不開。
2. **除法只寫一次。** 回測讀本機 CSV、模擬交易讀 WebSocket、暖機讀 REST ——
   如果比例在解析時算，同一個除法會有三份實作，三份都可能對 `volume == 0`
   或對報價幣／基礎幣的選擇做出不同決定，而「同一份策略在四種模式下的決策完全一樣」
   是這個專案的核心承諾。除法放在 DSL 讀取端只有一份，走哪個資料來源都是同一條路徑。

### 為什麼只存基礎幣的 taker 量，不存報價幣的兩欄

Binance 給四個數字（`q`、`n`、`V`、`Q`），這裡只收 `n` 和 `V`：

- 比例用 `V / volume`（兩邊同為基礎幣，`volume` 已經在 `Bar` 裡），和 `Q / q` 的差異
  只在加權方式（`Q/q` 是成交額加權），對一根 K 線內的訊號強度判斷沒有實質差別。
- 不收 `q`（報價幣總成交額）：`volume × close` 是夠用的近似（單根 K 線內誤差在
  高低價區間內，通常 <0.5%），而真要做「成交額 > 100 萬 USDT 才進場」這種濾網，
  這個近似就夠。**真的需要精確值時再加第三個欄位，欄位數版控（§6）已經留好路。**
- 不收 `ignore`（永遠是 0）、不收 `close_time`（`Bar::close_time(interval)` 已經算得出來）。

這是 YAGNI：現在加的兩個欄位剛好覆蓋 CEO 指名的兩個訊號（成交筆數、主動買盤佔比），
且不堵死未來（CVD 與精確成交額都還算得出來或還加得進來）。

### 型別選擇

```rust
/// 一根 K 線的訂單流：Binance 在每根 K 線裡附的成交結構資訊。
///
/// `Bar` 的 `Option<OrderFlow>` 是 `None` 代表「這筆資料的來源沒有提供訂單流」
/// （6 欄的舊格式本機檔、測試用的合成 K 線）。
/// 「這根 K 線真的沒有成交」是 `Some(OrderFlow { trades: 0, taker_buy_volume: 0.0 })`
/// ——Binance 對完全沒成交的分鐘確實會發這種 K 線（開高低收都等於前收、量為 0）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrderFlow {
    /// 這根 K 線內的成交筆數（Binance CSV 第 9 欄、WebSocket `k.n`）。
    pub trades: u64,
    /// 主動買方（taker buy）成交量，**以基礎幣計，和 `Bar::volume` 同單位**
    /// （CSV 第 10 欄、WebSocket `k.V`）。
    ///
    /// 和 `volume` 一樣用 `f64`：它只用來產生訊號與統計，不參與任何下單數量計算。
    pub taker_buy_volume: f64,
}
```

**`derive(Copy)` 不是可選的。** `Bar` 現在是 `Copy`（`bar.rs:95`），
`find_gaps` 的 `bars[i]`、回測迴圈、`metrics` 到處依賴它。
`OrderFlow` 少了 `Copy`，`Bar` 就會失去 `Copy`，改動會從「26 行」炸成「一堆 borrow 錯誤」。
**這是本次實作最可能爆開的單一細節。**

`trades` 用 `u64` 而不是 `u32`：對應 Binance JSON 的整數，不需要做任何範圍論證
（`u32` 也夠 —— BTCUSDT 日線約 500 萬筆，離 42.9 億還很遠 —— 但省下的 8 bytes
在一個已經有 f64 的 struct 裡是雜訊，不值得換來一次「會不會爆」的思考）。

### `Bar` 的大小變化（誠實算一次）

| | 現在 | 之後 |
|---|---|---|
| `Bar` | 8 + 4×8 + 8 = **48 bytes** | + `Option<OrderFlow>`（8+8＋判別位，對齊後 24）= **72 bytes**（+50%） |
| 1 年 1m K 線（525,600 根） | 25 MB | 38 MB |
| 1 天 1s K 線（86,400 根） | 4.1 MB | 6.2 MB |
| 1 年 1s K 線（31.5M 根） | 1.5 GB（本來就不可行） | 2.3 GB（還是不可行） |

+50% 不改變任何「做得到／做不到」的分界：原本放得進記憶體的規模還是放得進，
原本就不可行的（一整年的秒 K）本來也不可行。

---

## 5. 問題三：`Option` 還是強制有值？

### 兩個陷阱必須分開

這題的真正難點不是「要不要 `Option`」，是**兩種「沒有值」不能混**：

| 狀態 | 意思 | 策略應該怎麼反應 |
|---|---|---|
| 來源沒提供 | 6 欄舊檔、合成測試 K 線 | 「我不知道」→ 不可以當成 0 |
| 這根真的沒成交 | Binance 對無成交分鐘發的 K 線（`n=0, v=0`） | 「主動買盤佔比不存在」（0 ÷ 0）→ 也是不可以當成 0 |

### 陷阱 1：非 `Option`，用 `0` 當「沒資料」

`trades: u64 = 0` 與 `taker_buy_volume: f64 = 0.0`。

**這是三個方案裡唯一會造成金錢損失的。** 一支策略寫「主動買盤佔比 < 0.3 時做空」，
餵進一份 6 欄舊檔，每一根的佔比都被算成 `0 / 0`（或被預設成 `0`）→
**每一根都滿足「< 0.3」→ 整段回測一路做空**，而且回測會跑完、會出績效數字、
看起來完全正常。使用者拿這份績效去開真倉。

`Fixed::ZERO`／`0` 這種「看起來很合理的預設值」在訊號層是陷阱，因為
**`0` 在訂單流語意裡是一個有意義的極端值**（「完全沒有主動買盤」＝強烈賣壓），
不是中性值。沒有任何數字可以當「沒資料」的預設值。**否決。**

### 陷阱 2：兩個平行的 `Option`

```rust
pub trades: Option<u64>,
pub taker_buy_volume: Option<f64>,
```

型別上允許 `trades: None, taker_buy_volume: Some(5.0)` —— 一個檔案格式表示不出來、
現實不存在的狀態。不會造成錯訊號（讀不到的欄位走三值邏輯→空手），但它是
一個靠「大家都記得一起設」維持的約定，而且與 §6 的「6 欄或 8 欄，全有或全無」
檔案格式不同構。

### 決定：一個 `Option<OrderFlow>`

```rust
// 來源沒提供訂單流（6 欄舊檔、合成測試 K 線）
Bar { ..., order_flow: None }

// 這根真的沒成交（Binance 的無成交 K 線）
Bar { ..., volume: 0.0, order_flow: Some(OrderFlow { trades: 0, taker_buy_volume: 0.0 }) }

// 正常
Bar { ..., volume: 12.345, order_flow: Some(OrderFlow { trades: 150, taker_buy_volume: 6.0 }) }
```

- 兩種「沒有值」在型別上不同，不靠約定。
- 和檔案格式同構：`None` ↔ 6 欄，`Some` ↔ 8 欄，一行一個決定。
- 「主動買盤佔比」的 `None` 不需要存起來 —— 它是 `taker_buy_volume / volume` 在
  `volume == 0` 時的自然結果，由 DSL 讀取端產生（§7.1）。**不需要巢狀 `Option`。**
- 26 處字面建構補一行 `order_flow: None`，語意正確（那些都是合成 K 線，真的沒有訂單流）。

### `validate()` 的新檢查

`Bar::validate()`（`bar.rs:135-150`）加一個分支，新錯誤 `BarError::BadOrderFlow`：

```text
order_flow 是 Some 時：
  taker_buy_volume 必須是有限數字           （和既有 BadVolume 同一個理由）
  taker_buy_volume >= 0
  taker_buy_volume <= volume                （主動買不可能超過總量）
```

`taker_buy_volume <= volume` 這條安全嗎？會不會誤殺真實資料？不會：
`V` 和 `v` 都是交易所給的十進位字串，而 Rust 的 f64 解析是單調的
（`a ≤ b` 的十進位字串解析後仍 `≤`）。所以解析後出現 `V > v`
一定代表來源資料本身不一致（手改檔案、寫檔中途崩潰），不是浮點誤差。

**刻意不檢查的**：`trades == 0 ⇒ volume == 0`。Binance 官方 CSV 真的有
`n=0, v=0` 的無成交 K 線，而現有 `validate()` 已經接受 `volume == 0`
（`bar.rs:146`），加這條只會多一個現實中不會發生的分支。

---

## 6. 問題四：本機存檔格式怎麼升版

### 現狀

`bar_store.rs` 是**本專案自有格式**（不是 Binance 格式），6 欄、無 header、一行一根，
`parse_bars` 是純粹的 `lines().map(parse_line)`，錯誤訊息帶行號。

### 方案比較

| | 判版方式 | 優點 | 缺點 |
|---|---|---|---|
| V1 | **檔案第一行寫版本號** | 版本意圖明確 | 破壞「每一行都是一根 K 線」的不變量；行號錯誤訊息要全部 −1；空檔案要特別處理；`parse_bars` 從 map 變成有狀態的解析 |
| V2 | **用欄位數判版（選定）** | 零新增語法；每一行自我描述；`parse_bars` 保持 map；空檔案照樣是空陣列 | 欄位數只能表達「有多少欄」，不能表達「同樣 8 欄但語意改了」 |
| V3 | **新檔名／新副檔名**（`.v2.csv`） | 老執行檔不會誤讀 | 要改 1.7 下載器的路徑規則、要處理兩種檔名並存、使用者資料夾變亂 |

### 決定：V2（欄位數判版）

```text
6 欄 → open_time, open, high, low, close, volume              → order_flow: None
8 欄 → 上述 + trades, taker_buy_volume                        → order_flow: Some(..)
其他 → BarStoreError::WrongFieldCount（錯誤訊息要同時提到 6 與 8 都可以）
```

V3 的「老執行檔不會誤讀」是唯一真正的優點，但代價是檔名規則和使用者資料夾，
而 V2 在老執行檔上的行為已經足夠好：**老執行檔讀到 8 欄檔會
`WrongFieldCount { found: 8 }` 大聲失敗，不會靜默誤讀**（因為它要求剛好 6 欄）。
大聲失敗 + 資料可重新下載 = 可接受的前向不相容（見 §8.5）。

### 三條規則

1. **讀：欄位數決定語意。** 6 欄 → `order_flow: None`；8 欄 → 解析後走 `Bar::validate()`
   （所以壞掉的訂單流會在這裡被攔下，和既有 `InvalidBar` 同一條路）。
2. **同一個檔案不可混欄數。** 第一個非空行的欄位數決定這個檔案的欄數，
   後面任何一行不同就是 `WrongFieldCount` 錯誤。
   理由：半欄數的檔案（寫檔中途崩潰、手工合併兩個來源）會變成
   「一段有訂單流、一段沒有」的資料集，而用訂單流的策略在沒有的那段會一路空手 ——
   **一份靜默少交易的回測**。三行程式碼把它變成一個講得清楚的錯誤。
3. **寫：全部 `Bar` 都有訂單流才寫 8 欄，否則寫 6 欄。**
   - `format_bars` 的規則就是一個 `.all(|b| b.order_flow.is_some())`。
   - 唯一的生產呼叫端（`at-downloader`）的資料一定同源，所以實務上永遠寫 8 欄。
   - 好處：既有的 golden 文字測試
     `bar_store::tests::format_is_stable_and_predictable`（斷言精確的 6 欄輸出字串）
     **不用改也會綠**，因為它的 `Bar` 是 `order_flow: None`。
   - 未來若真有混來源的呼叫端：拆成兩個檔案，不要教這個格式交錯。

### 問題二的答案：舊檔「讀得到」+「用得到的時候才報錯」

> 要不要求使用者重新下載？

**不要求。** 兩個理由：

1. **成本不對稱。** 支援舊檔的成本是 `parse_bars` 裡一個 `match fields.len()`（約 3 行）。
   要求重新下載的成本是：使用者所有既有資料全部作廢、跑一支沒用到訂單流的
   均線回測也要先等下載 ——換來 0 個好處。
2. **repo 內的測試就依賴它。** `BTCUSDT-1d-2024-01.csv`（6 欄）被 2.8 的
   golden 曲線測試逐點比對，那份檔案不能重新產生。

> 既然資料可以重新下載，為什麼不乾脆要求？

因為「可以重新下載」解決的是**資料遺失風險**，不是**使用體驗**。
這個專案還在邊做邊學的階段，使用者的既有資料是他花時間下載、而且他記得內容的東西；
一次版本升級讓所有舊資料報錯，是在教使用者「升級很痛」。

> 那麼「策略要訂單流、但資料是舊格式」怎麼辦？

這是這份設計**唯一真正的新失效模式**，用 §7.2 的 `needs_order_flow()` 硬錯誤處理。
絕對不可以讓它靜默跑完。

### Binance 原始 12 欄 CSV（`kline_csv.rs`）

只改 `parse_line`：多讀 `fields[8]`（`trades`）與 `fields[9]`（`taker_buy_volume`），
一律產生 `Some(OrderFlow)`。欄位數檢查**維持 `!= 12`**（Binance 的現貨與
U 本位合約歷史 CSV 都是 12 欄；少欄就是來源變了，寧可大聲失敗）。
`fields[7]`（`quote_asset_volume`）、`fields[10]`、`fields[11]` 繼續忽略，
doc comment 的「之後真的要用時再加」改成記錄現在用了哪兩欄、為什麼不收另外兩欄（§4）。

---

## 7. 問題五：DSL 引擎側怎麼讀到新欄位

### 7.1 現狀與最小改動

DSL 的數值輸入只有三種 `Expr`：`Price`、`Number`、`Indicator`
（`strategy_dsl/mod.rs:144-168`）。`Expr::Price` 編譯成 `Node::Price { field, offset }`，
求值走一個函式（`strategy_dsl/eval.rs:600-610`）：

```rust
fn price_field(bar: &Bar, field: PriceField) -> Option<Fixed> {
    match field {
        PriceField::Open => Some(bar.open),
        ...
        PriceField::Volume => volume_to_fixed(bar.volume),   // f64 → Fixed 的確定性轉換
    }
}
```

**這就是唯一的入口，而且它的回傳型別已經是 `Option<Fixed>`。**
`None` 在 DSL 裡的語意已經定義好了：「無法判定」→ 三值邏輯傳播 → 空手
（`mod.rs:237-252`、`eval.rs` 的 `step_held`）。
新欄位的「沒資料」要的語意，和既有的「暖機不足／指標算不出來」完全同一套。

### 決定：加兩個 `PriceField` variant，不加指標

```rust
pub enum PriceField {
    Open, High, Low, Close, Volume,
    /// 這根 K 線的成交筆數。來源沒提供訂單流時無法判定（→ 空手）。
    Trades,
    /// 主動買盤佔比 = 主動買方成交量 ÷ 總成交量，範圍 0～1。
    /// 來源沒提供、或這根完全沒成交（分母為 0）時無法判定。
    TakerBuyRatio,
}
```

JSON 值（snake_case，照 `mod.rs:58-61` 的既有規則）：`"trades"`、`"taker_buy_ratio"`。

求值（`price_field` 兩個新 match arm，約 8 行）：

```text
Trades        → bar.order_flow 取不到 → None
                 i64::try_from(trades) → Fixed::from_int(..)   // 溢位 → None
TakerBuyRatio → bar.order_flow 取不到 → None
                 bar.volume 不是 > 0.0 → None                   // 0 與 NaN 都在這裡擋掉
                 volume_to_fixed(taker_buy_volume / bar.volume)
```

### 為什麼是 `PriceField` 而不是新的 `IndicatorName`

- 這兩個是**每根 K 線的原始讀取、無狀態**，`IndicatorKind` 是給有狀態的 rolling window 用的。
- `PriceField` 這個型別名稱已經不精確（它裝著 `Volume`），但**不改名**：
  `Expr::Price` 的 variant 名稱決定序列化出來的 `"kind": "price"`，
  改名會讓使用者已存檔的 `schemaVersion: 1` 策略讀不回來。型別名稱的精確度不值這個代價。

### 這個選擇免費帶來的東西（這是本節的重點）

因為掛在 `Expr::Price` 上，**不用寫任何額外程式碼**就得到：

| 寫得出來的策略 | 為什麼免費 |
|---|---|
| `taker_buy_ratio > 0.55` | `Cond::Gt` + `Expr::Number` |
| `cross_above(taker_buy_ratio, 0.5)`（買壓翻多） | `Cond::CrossAbove` 吃任何兩個 `Expr` |
| `sustained(taker_buy_ratio > 0.6, 3 根)` | `Cond::Sustained` |
| `sma(source: taker_buy_ratio, period: 20)`（20 根平均買壓） | `Expr::Indicator.source` 本來就吃任何 `Expr`（`mod.rs:157-159`） |
| `ema(source: trades, 50)` 當活躍度濾網 | 同上 |
| `taker_buy_ratio[offset: 1]`（上一根的買壓） | `Expr::Price.offset` |
| `all([ cross_above(sma10, sma50), taker_buy_ratio > 0.55 ])`（均線訊號＋買壓確認） | `Cond::All` |

兩個 enum variant ＋ 兩個 match arm，換到上面整張表。**這是方案 A 的主要報酬。**

暖機計算（`eval.rs` 的 `warmups()`）不用改：`Node::Price` 的暖機是 `1 + offset`，
新 variant 照用。

### 7.2 必須配套的防護：`needs_order_flow()`

**沒有這一塊，整份設計會製造一個比現狀更糟的失效模式。**

場景：使用者做了一支「主動買盤佔比 > 0.6 才進場」的策略，拿 1.7 下載器三個月前
存的 6 欄檔回測。`price_field` 每一根都回 `None` → 三值邏輯 → 每一根空手 →
**回測跑完、零筆交易、績效全 0**。畫面上看起來像「這支策略不會進場」，
實際上是「資料裡沒有這個欄位」。使用者會去改策略參數，改到放棄。

處置：在 `Strategy` trait 加一個預設方法（和既有的 `warmup_bars()` 同一個形狀 ——
「這只是宣告，不是保護」）：

```rust
pub trait Strategy {
    fn on_bar(&mut self, bar: &Bar) -> TargetPosition;
    fn warmup_bars(&self) -> usize { 0 }

    /// 這支策略會不會讀 K 線的訂單流欄位（成交筆數、主動買盤佔比）。
    ///
    /// 和 `warmup_bars()` 一樣只是宣告：回 `true` 的策略餵到沒有訂單流的
    /// K 線時仍然必須安全地回傳空手。呼叫端用它在**跑之前**就擋下
    /// 「策略要訂單流、資料沒有」這種會靜默產生零交易回測的組合。
    fn needs_order_flow(&self) -> bool { false }
}
```

- 預設 `false` → **四個內建策略、`LeveragedStrategy` 零改動**。
- `CustomStrategy`：`compile()` 時掃一次攤平後的 `nodes`
  （已經是一個 `Vec<Node>`，一個 `.iter().any(..)` 就夠），結果存成欄位。
- `LeveragedStrategy` 要轉交內層的答案（和它轉交 `warmup_bars()` 同一行寫法）。

呼叫端（三處，都是「跑之前」）：

| 位置 | 行為 |
|---|---|
| `app/src-tauri/src/backtest.rs`（`read_bars_file` 之後） | 策略要訂單流但 `bars` 的 `order_flow` 是 `None` → 回傳錯誤：<br>「這份策略用到訂單流（成交筆數／主動買盤佔比），但本機的 K 線檔是舊格式（6 欄）。請到『資料下載』重新下載 BTCUSDT 1m。」 |
| 模擬交易／測試網的啟動路徑（暖機 K 線抓回來之後） | 同上判斷，訊息改成暖機資料來源 |
| 策略編輯器（可選，🟢） | `needs_order_flow()` 為真時在 UI 標示「這支策略需要訂單流資料」 |

檢查「`bars` 有沒有訂單流」用第一根就夠 —— §6 的規則 2 已經保證同一個檔案不混欄數。

### 7.3 其他要同步改的解析器

| 檔案 | 改什麼 |
|---|---|
| `crates/market-stream/src/lib.rs:362-394` | `RawKline` 加 `#[serde(rename = "n")] trades: u64` 與 `#[serde(rename = "V")] taker_buy_volume: String`。**`rename` 是必須的**：`v`（總量）已經佔掉小寫，Rust 不能也不該有一個叫 `V` 的欄位。`RawKline` 沒有 `deny_unknown_fields`，所以 `q`／`Q`／`B` 繼續被忽略。`kline_event()` 組 `Some(OrderFlow)`。 |
| `crates/binance-client/src/market_data.rs:103-134` | REST `/api/v3/klines`（暖機來源）：`row.len() >= 10` 時讀 `row[8]`／`row[9]` 組 `Some`，否則 `None`。既有的 `row.len() < 6` 守門不動。 |

**兩邊對「欄位缺失」的處置刻意不同，這是繼承既有程式碼而不是新發明的：**

- WebSocket：`V` 解析不出來 → 整則訊息 `None`（跳過），和既有 `v` 解析失敗的處置完全一樣
  （`lib.rs:387-394` 的 `?`）。後果是「行情完全安靜」——**大聲、使用者馬上發現**。
- REST：欄位不足 → `order_flow: None`，然後由 `needs_order_flow()` 擋下。
  後果也是明確的錯誤訊息，不是靜默空手。

兩種都不會變成「策略看起來在跑但永遠不進場」。

### 7.4 確定性（必須保持的承諾）

「同一份策略在回測／模擬／測試網的決策完全一樣」靠兩件事維持：

1. **除法只有一份實作**（§4）：`taker_buy_volume / bar.volume` 只在 `price_field` 出現一次。
2. **f64 → `Fixed` 走既有的 `volume_to_fixed`**（`eval.rs:623-634`，截尾不四捨五入，
   超出範圍回 `None` 不飽和）。IEEE-754 除法的結果對同樣的輸入在每個平台都是同一組 bit，
   截尾後也是同一個 `Fixed`。

精度注意：比例截到 `1e-8`（`Fixed::SCALE`），所以 `0.6` 可能落在 `0.59999999`。
DSL **沒有 `==` 運算子**（只有 `gt/gte/lt/lte/cross_*`），所以沒有暴露面；
門檻式比較差一個 `1e-8` 在訊號語意上沒有意義。

`f64` 的使用是否違反專案規範？`CLAUDE.md` 寫的是「不可用 `f64` 做**下單相關計算**」。
`taker_buy_volume` 和既有的 `volume` 完全同一個角色：只產生訊號、只在
`volume_to_fixed` 之後進入 `Fixed` 域做比較，**不乘進任何下單數量**。
下單數量仍然全程走 `Fixed` 與 `SymbolRules`。這是沿用既有先例，不是新開一個洞。

---

## 8. 魔鬼代言人：失效模式與攻擊清單

### 8.1 資料量 10 倍／100 倍

`Bar` 48 → 72 bytes（+50%）。本機檔案一行約 50 → 65 bytes（+30%，1 年 1m 檔 26MB → 34MB）。
記憶體見 §4 的表：沒有任何「原本可行變成不可行」的分界被跨過。
`parse_bars` 多解析 2 欄，解析時間約 +25%，而它本來就不是瓶頸（回測迴圈才是）。
**撐得住。**

### 8.2 第三方（Binance）改欄位／掛掉

| 情況 | 行為 |
|---|---|
| 歷史 CSV 不再是 12 欄 | 既有的 `!= 12` 檢查就報錯（行號 + 實際欄數），不會誤讀 |
| WebSocket 不再送 `n`／`V` | 整則 kline 訊息被跳過 → 行情完全安靜 → 使用者立刻發現（見 §7.3） |
| REST klines 回傳變短 | `order_flow: None` + `needs_order_flow()` 的硬錯誤 |
| Binance 掛掉 | 和現在完全一樣（不是這份改動引入的） |

### 8.3 資料不一致（這份設計唯一真正的新風險）

**策略要訂單流 + 資料是舊格式 → 靜默零交易回測。** 這是整份設計最危險的一點，
也是 §7.2 `needs_order_flow()` 存在的唯一理由。
實作驗收必須包含一個測試：「用訂單流的 DSL 策略 + 6 欄 bars → 載入端回傳錯誤，
而不是回傳一份零交易的回測結果」。

次要的一致性風險：半欄數檔案 → §6 規則 2（同檔不可混欄數）擋下。

### 8.4 單人維護

新增 1 個型別（`OrderFlow`）、1 個 trait 預設方法、2 個 enum variant、
1 個錯誤 variant、**0 個新依賴、0 個新 crate、0 個新執行路徑**。
沒有新的抽象層、沒有泛型、沒有需要讀兩個檔案才看得懂的機制。
欄位數判版是「看一行有幾個逗號」，壞檔的錯誤訊息帶行號。

### 8.5 回滾

- **程式回滾、資料留著**：舊執行檔讀新的 8 欄檔 → `WrongFieldCount { found: 8 }`，
  **大聲失敗，不會誤讀成別的數字**。要繼續用舊執行檔必須重新下載（資料是公開可重下的）。
  這是已知的前向不相容，寫進步驟文件的「怎麼驗證」段落。
- **資料回滾**：新執行檔讀 6 欄舊檔 → 正常運作（不用訂單流的策略）、
  明確錯誤（要用訂單流的策略）。這是 §6 的主要設計目標。
- **DSL 策略回滾**：`schemaVersion` 不變（只加了 `PriceField` 的 enum 值）。
  舊引擎讀到 `"field": "taker_buy_ratio"` → serde 認不得 → 反序列化錯誤，
  不會默默當成別的欄位。**不升 `SCHEMA_VERSION`**：舊策略在新引擎上行為完全不變，
  升版只會讓使用者的既有策略全部報「來自不同版本」。

### 8.6 安全／被攻擊面

新增的是兩個數字欄位的解析。`compile()` 的信任邊界不變（`PriceField` 沒有參數可驗）、
`MAX_NODES`／`MAX_DEPTH`／`MAX_PERIOD` 不變、DSL 仍然只輸出 `TargetPosition`、
`Node` 仍然沒有任何帶動作語意的 variant。
`u64` 與 `f64` 解析失敗都走既有的 `BadNumber`／跳過路徑，沒有 `unwrap`。
記憶體放大：`trades` 不影響任何 `Vec` 的容量（只有 `period` 會，而它沒變）。
**沒有新的攻擊面。**

### 8.7 流量 100 倍

不適用（這是本機桌面工具，沒有對外服務）。即時串流側：`RawKline` 多兩個欄位的
反序列化，對每秒一則的訊息完全無感。

### 8.8 誠實說撐得住的地方（不為了湊數捏造問題）

實際查證過、diff 必須是空的：

- **`at-session-store` 完全不受影響。** `curve.csv` 是 `open_time,equity` 兩欄
  （`curve.rs:1`），`session-store` 整個 crate 連 `Bar` 都沒 import（grep 零結果）。
- **四個內建策略零改動。** `bollinger.rs`／`donchian.rs`／`rsi.rs`／`sma_cross.rs`
  裡沒有任何 `Bar {` 字面建構，它們只讀 `bar.close`／`bar.high`／`bar.low`。
- **`at-paper-trading`／`at-testnet-trading` 的生產程式碼不建構 `Bar`**
  （只有測試輔助函式建構）。它們從 `KlineUpdate.bar` 拿 K 線，所以
  **訂單流會自動流到模擬交易與測試網的策略，不需要為它們寫任何東西。**
- **`at-engine`／`at-risk-control`／`at-portfolio-risk`／`at-secret-store`／
  `at-account-sync` 零改動。**
- **`Strategy` trait 的 `on_bar` 簽名不動** → `WarmupBars::replay`、
  `run_backtest`、`PaperEngine`、測試網 `Trader` 的呼叫端一行都不用改。
- **既有 golden 測試不用改也會綠**：`format_is_stable_and_predictable`（6 欄輸出字串）
  與 2.8 的 `integration_sma_cross_validation`（6 欄 fixture + golden 權益曲線），
  因為 §6 規則 3（全有才寫 8 欄）與 §6 規則 1（6 欄讀成 `None`）。

---

## 9. 對現有程式碼的影響範圍

### 9.1 不改（diff 必須是空的）

```text
crates/engine/            crates/risk-control/      crates/portfolio-risk/
crates/secret-store/      crates/account-sync/      crates/session-store/
crates/core/src/strategies/{bollinger,donchian,rsi,sma_cross}.rs
crates/core/src/{fees,rules,types,fixed,metrics}.rs   ← metrics 只有測試輔助要補欄位
crates/core/src/strategy_dsl/{equivalence,serialization}.rs  ← 只加測試，不改邏輯
```

### 9.2 修改（按依賴順序）

| 檔案 | 改動 | 規模 |
|---|---|---|
| `crates/core/src/bar.rs` | `OrderFlow` 型別、`Bar.order_flow` 欄位、`validate()` 新分支、`BarError::BadOrderFlow` | ~40 行 |
| `crates/core/src/kline_csv.rs` | `parse_line` 讀 `fields[8]`／`[9]`；doc comment 改寫 | ~15 行 |
| `crates/core/src/bar_store.rs` | 6／8 欄讀寫、同檔混欄數拒絕、錯誤訊息、doc comment | ~50 行 |
| `crates/core/src/strategy.rs` | `needs_order_flow()` 預設方法、`LeveragedStrategy` 轉交 | ~12 行 |
| `crates/core/src/strategy_dsl/mod.rs` | `PriceField` 兩個 variant | ~10 行 |
| `crates/core/src/strategy_dsl/eval.rs` | `price_field` 兩個 arm、`CustomStrategy::needs_order_flow` | ~20 行 |
| `crates/market-stream/src/lib.rs` | `RawKline` 兩個欄位、`kline_event` 組 `OrderFlow` | ~12 行 |
| `crates/binance-client/src/market_data.rs` | `parse_row` 讀 `row[8]`／`[9]`（`len >= 10` 時） | ~12 行 |
| `app/src-tauri/src/backtest.rs` | 載入後的 `needs_order_flow()` 檢查 + 繁中錯誤訊息 | ~15 行 |
| 模擬交易／測試網啟動路徑 | 暖機資料的同一個檢查 | ~15 行 |

### 9.3 要補欄位的字面建構處（實測）

`grep` 實測：**26 處** `Bar { open_time: .. }` 字面建構，散在 **18 個檔案**：

| 檔案 | 處數 | 性質 |
|---|---|---|
| `core/src/strategy_dsl/tests.rs` | 2 | 測試輔助 |
| `core/src/strategy_dsl/indicators.rs` | 2 | 測試輔助 |
| `core/src/strategies/mod.rs` | 2 | 測試輔助 |
| `core/src/kline_csv.rs` | 2 | **1 處是 `parse_line`（要填真值）**、1 處測試期望值 |
| `core/src/bar.rs` | 2 | 測試輔助 |
| `core/src/bar_store.rs` | 2 | **1 處是 `parse_line`（要填真值或 `None`）**、1 處測試輔助 |
| `core/src/backtest.rs` | 2 | 測試輔助 |
| `market-stream/src/lib.rs` | 1 | **生產碼（要填真值）** |
| `binance-client/src/market_data.rs` | 1 | **生產碼（要填真值）** |
| `core/src/{warmup,strategy,metrics}.rs`、`core/src/strategy_dsl/equivalence.rs`、`downloader/src/lib.rs`、`paper-trading/src/lib.rs`、`testnet-trading/src/{lib,trader}.rs`、`app/src-tauri/src/backtest.rs` | 各 1 | 測試輔助／測試資料 |

**實際編輯量比「26 處」小：** 其中 **4 處是真正要填值的解析器**，其餘 22 處
是各檔案的本地 `fn bar(..)`／`fn ohlc_bar(..)` 測試輔助函式，一個檔案通常只要
改那一個輔助函式、補一行 `order_flow: None`。**約 20 個編輯點、其中 16 個是一行。**

> `app/src-tauri/src/{paper_trading,testnet_trading}.rs` 裡的 `Bar { snapshot }`
> 是事件 enum 的 variant，不是 `at_core::Bar`，**不受影響**（別被 grep 誤導）。

編譯器會逐一指出每一處漏補的地方，漏一處就編不過 —— 這是方案 A 的安全網。

### 9.4 新增測試（驗收條件）

| 測試 | 驗什麼 |
|---|---|
| `bar.rs` | `taker_buy_volume > volume` → `BadOrderFlow`；`trades: 0, volume: 0.0` → 合法（無成交 K 線）；`order_flow: None` → 合法 |
| `kline_csv.rs` | 官方 12 欄範例行解析出 `trades: 13, taker_buy_volume: 401.82` |
| `bar_store.rs` | 6 欄檔 → `order_flow: None`；8 欄檔 round-trip 完全相等；混欄數檔 → 錯誤；全 `None` 的 bars → 寫出 6 欄 |
| `market-stream` | 既有的 `KLINE_OPEN`／`KLINE_CLOSED` 常數**已經含 `n`／`V`／`q`／`Q`**（`lib.rs:451-469`），直接加斷言即可 |
| `strategy_dsl/tests.rs` | `taker_buy_ratio` 在 `order_flow: None` 時 → 空手；`volume: 0.0` 時 → 空手；正常值的比例正確；`sma(source: taker_buy_ratio)` 能跑 |
| `strategy_dsl/serialization.rs` | `"trades"`／`"taker_buy_ratio"` 的 JSON 名稱鎖住 |
| **載入端（最重要）** | 用訂單流的策略 + 6 欄 bars → **回傳錯誤**，不是零交易的回測結果 |
| 回歸 | 2.8 的 `integration_sma_cross_validation` 不改也要綠 |

### 9.5 實作順序（每一步都能獨立驗證）

1. `bar.rs`：`OrderFlow` + 欄位 + `validate` + 測試。**這一步會引爆 26 處編譯錯誤，
   一次補完**（22 處 `order_flow: None`，4 處暫時也填 `None`）。驗收：`cargo test` 全綠。
2. `kline_csv.rs`：讀 `[8]`／`[9]`。驗收：官方範例行的新斷言。
3. `bar_store.rs`：6／8 欄讀寫 + 混欄數拒絕。驗收：round-trip 與舊檔測試、2.8 整合測試。
4. `market-stream` + `binance-client`：即時與 REST 來源。驗收：既有測試常數的新斷言。
5. DSL：`PriceField` 兩個 variant + `price_field`。驗收：三值邏輯測試 + serialization 測試。
6. `needs_order_flow()` + 載入端硬錯誤。驗收：§9.4 最後兩列。

每一步都要過 `cargo fmt --all`、`cargo clippy --all-targets -- -D warnings`、`cargo test`，
並在 `docs/steps/` 補一份說明（照 `CLAUDE.md` 的流程）。

---

## 10. 未決問題

### 10.1 🟡 要不要同時收報價幣的成交額（`quote_asset_volume`）

建議**先不收**（§4）：`volume × close` 的近似對成交額濾網夠用，
真要精確值時加第三個欄位升到 9 欄，欄位數版控已經留好路。
如果 CEO 的高頻策略明確需要「USDT 成交額」門檻（而不是「基礎幣量」門檻），
在實作前講一句就補上，成本是 1 個欄位 + 1 欄 CSV。

### 10.2 🟡 CVD（累積成交量差）要不要現在做

資料已經存得下（`2 × taker_buy_volume − volume`），但 DSL 的 `Expr` 沒有算術運算子，
所以 CVD 需要一個新的**有狀態指標節點**（累加器），那是獨立的一件事。
**這份設計刻意只保證「資料存得下」，不實作 CVD。**

### 10.3 🟡 混欄數檔案要容忍還是拒絕

本文件選「拒絕」（§6 規則 2）。容忍的話程式少三行，但會換來一種
「一段有訂單流一段沒有」的靜默少交易回測。建議照本文件。

### 10.4 🟢 `needs_order_flow()` 要不要上 UI

策略編輯器顯示「這支策略需要訂單流資料」是加分，不是這次的驗收條件。

### 10.5 給 CEO 的決策點

**沒有 🔴。** 這份改動全部落在 🟡（內部資料模型 + 本機自有檔案格式升版）：
不新增依賴、不新增 crate、不碰認證／付費／下單路徑、不動 DB schema
（這個專案沒有 DB）、不改 `SCHEMA_VERSION`。

需要**報告**（不需要等同意）的一件事：
**重新下載過的本機 K 線檔會變成 8 欄，用舊版執行檔讀會報錯**（§8.5）。
既有的 6 欄檔案繼續可讀，所以不會有資料遺失，而且這些都是
`at-downloader` 可以重新下載的公開歷史資料。

---

## 11. 後果

### 得到什麼

- 真實的訂單流訊號（成交筆數、主動買盤佔比）進到**回測、模擬交易、測試網**三條路徑，
  **不需要抓 tick 資料、不需要新的資料來源、不需要新依賴**。
- DSL 用兩個 enum variant 換到一整張策略寫法表（§7.1），包含
  「買壓佔比的移動平均」、「買壓翻多穿越」、「連續 N 根買壓偏多」、
  「均線訊號 + 買壓確認」這些真正屬於高頻／微觀結構的條件。
- 「訊號在 K 線收盤產生、下一根開盤成交」的執行模型**一行都沒動**。

### 付出什麼

- `Bar` 從 48 變 72 bytes（+50%），只在一整年的秒 K 這種本來就不可行的規模才有感。
- 26 處字面建構補一行（約 20 個編輯點），編譯器全程指路。
- 本機檔案格式從 6 欄變 8 欄：向後相容（讀得到舊檔）、**前向不相容但大聲失敗**
  （舊執行檔讀不了新檔）。
- 多一條必須守住的規則：**用訂單流的策略碰到沒有訂單流的資料，必須是錯誤，
  不可以是一份零交易的回測。** 這條規則靠 `needs_order_flow()` 與載入端的檢查落地，
  是實作驗收裡最重要的一項。

### 不做什麼（明確劃掉）

tick／逐筆成交資料、CVD 指標、報價幣成交額、積木 UI 怎麼呈現這些指標、
`Interval::S1`、`PriceField` 改名、`SCHEMA_VERSION` 升版。
