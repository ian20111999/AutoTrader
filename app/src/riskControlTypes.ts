// 跟 app/src-tauri/src/risk_control.rs 的 BreakerRuleDto/RiskEventDto/
// RiskStatusDto/SessionExposureDto 對應。金額欄位是 Fixed 的字串表示，
// 理由同 paperTradingTypes.ts（下單相關的數字不經過 JS 的 number）。

export interface BreakerRule {
  /** 穩定識別字串，開關時傳回後端（不要用顯示文字當 key）。 */
  id: string;
  /** 條件，例如「連續虧損 5 筆」。 */
  condition: string;
  /** 觸發後的動作，例如「暫停這個策略」。 */
  action: string;
  enabled: boolean;
  /** 強制開啟：UI 不可以給關閉的控制元件。 */
  mandatory: boolean;
  /** 這條規則的輸入與限制，直接顯示給使用者看。 */
  note: string;
}

export interface RiskEvent {
  seq: number;
  atMs: number;
  /** 「交易所時間」或「本機時間」。 */
  clock: string;
  session: string | null;
  symbol: string | null;
  message: string;
}

export interface SessionExposure {
  sessionId: string;
  kind: string;
  symbol: string;
  position: string;
  equity: string | null;
  dailyPnl: string | null;
  killSwitch: boolean | null;
  /** 這筆資料有多舊；null = 還沒有任何一根收盤 K 線。 */
  asOfMs: number | null;
  /** 非 null = 這場已經被熔斷擋住送單。 */
  breakerTrip: string | null;
}

export interface RiskStatus {
  takenAtMs: number;
  /** 非 null = 整個帳戶的下單已被熔斷暫停。 */
  accountPause: string | null;
  sessions: SessionExposure[];
}
