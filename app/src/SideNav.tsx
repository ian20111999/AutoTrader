import type { ReactElement } from "react";
import { SIDE_NAV_GROUPS, type PageId } from "./nav";

// 圖示照設計稿 SideNav.dc.html 的線條風格重繪（stroke-width 1.7、viewBox 24x24）。
// 「比較」在設計稿裡沒有獨立的側邊導覽項目（是從總覽/回測頁連結過去的），
// 這裡另外挑一個同風格的重疊圓圖示代表「比較」。
// disabled 的項目（部署/即時交易/高頻監控/風控）背後功能還沒做，
// 圖示先照設計稿分組邏輯挑同風格的線條圖代表，待功能做出來再換正式圖示。
// 策略編輯器（Phase F3）已經接上頁面，不再 disabled。
const ICONS: Record<string, ReactElement> = {
  overview: (
    <>
      <rect x="3" y="3" width="7" height="7" rx="1.5" />
      <rect x="14" y="3" width="7" height="7" rx="1.5" />
      <rect x="3" y="14" width="7" height="7" rx="1.5" />
      <rect x="14" y="14" width="7" height="7" rx="1.5" />
    </>
  ),
  strategies: (
    <>
      <path d="M12 3 L21 8 L12 13 L3 8 Z" />
      <path d="M3 13 L12 18 L21 13" />
    </>
  ),
  strategyEditor: (
    <>
      <path d="M4 20l1-4.5L16.5 4 20 7.5 8.5 19 4 20z" />
      <path d="M14 6.5L17.5 10" />
    </>
  ),
  backtest: (
    <>
      <path d="M3 12a9 9 0 1 0 3-6.7" />
      <path d="M3 4v5h5" />
      <path d="M12 7v5l3 2" />
    </>
  ),
  compare: (
    <>
      <circle cx="9" cy="12" r="6" />
      <circle cx="15" cy="12" r="6" />
    </>
  ),
  paperTrading: (
    <>
      <path d="M9 3h6M10 3v6l-5.5 9.5A2 2 0 0 0 6.2 21h11.6a2 2 0 0 0 1.7-2.5L14 9V3" />
      <path d="M7 15h10" />
    </>
  ),
  deployment: (
    <>
      <path d="M12 2v13" />
      <path d="M7 10l5-5 5 5" />
      <path d="M5 16l-2 6h18l-2-6" />
    </>
  ),
  // 測試網交易：跟模擬交易同一個燒瓶輪廓，加一個警示感的閃電，代表「這次是真的送單」。
  testnetTrading: (
    <>
      <path d="M9 3h6M10 3v6l-5.5 9.5A2 2 0 0 0 6.2 21h11.6a2 2 0 0 0 1.7-2.5L14 9V3" />
      <path d="M13 10l-3 4h2l-1 4 4-5h-2l1-3z" />
    </>
  ),
  liveTrading: (
    <>
      <path d="M13 2L4 14h6l-1 8 9-12h-6z" />
    </>
  ),
  highFrequency: (
    <>
      <circle cx="12" cy="12" r="1.5" />
      <path d="M8.5 15.5a5 5 0 0 1 0-7" />
      <path d="M15.5 8.5a5 5 0 0 1 0 7" />
      <path d="M5.5 18.5a9 9 0 0 1 0-13" />
      <path d="M18.5 5.5a9 9 0 0 1 0 13" />
    </>
  ),
  riskControl: (
    <>
      <path d="M12 3l7 3v6c0 4.5-3 7.5-7 9-4-1.5-7-4.5-7-9V6z" />
      <path d="M9.5 12l2 2 3.5-4" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3M12 19v3M4.2 4.2l2.1 2.1M17.7 17.7l2.1 2.1M2 12h3M19 12h3M4.2 19.8l2.1-2.1M17.7 6.3l2.1-2.1" />
    </>
  ),
};

interface SideNavProps {
  active: PageId;
  onSelect: (page: PageId) => void;
}

export function SideNav({ active, onSelect }: SideNavProps) {
  return (
    <nav className="side-nav" aria-label="主選單">
      <div className="side-nav__brand">
        <svg width="28" height="28" viewBox="0 0 28 28" aria-hidden="true">
          <rect width="28" height="28" rx="7" fill="var(--accent)" />
          <path
            d="M7 18 L12 12 L16 15 L21 9"
            fill="none"
            stroke="#1A1405"
            strokeWidth="2.4"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        <span>自動交易台</span>
      </div>
      {SIDE_NAV_GROUPS.map((group, groupIndex) => (
        <div className="side-nav__group" key={group.label ?? `group-${groupIndex}`}>
          {group.label && <div className="side-nav__group-label">{group.label}</div>}
          <div className="side-nav__items">
            {group.items.map((item) => (
              <button
                key={item.id}
                type="button"
                className={
                  item.id === active
                    ? "side-nav__item side-nav__item--active"
                    : "side-nav__item"
                }
                aria-current={item.id === active ? "page" : undefined}
                disabled={item.disabled}
                aria-disabled={item.disabled || undefined}
                title={item.disabled ? "即將推出" : undefined}
                onClick={() => {
                  if (!item.disabled) onSelect(item.id as PageId);
                }}
              >
                <svg
                  width="18"
                  height="18"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.7"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden="true"
                >
                  {ICONS[item.id]}
                </svg>
                <span>{item.label}</span>
                {item.disabled && <span className="side-nav__badge">即將推出</span>}
              </button>
            ))}
          </div>
        </div>
      ))}
    </nav>
  );
}
