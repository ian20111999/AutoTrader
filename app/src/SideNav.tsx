import type { ReactElement } from "react";
import { NAV_ITEMS, type PageId } from "./nav";

// 圖示照設計稿 SideNav.dc.html 的線條風格重繪（stroke-width 1.7、viewBox 24x24）。
// 「比較」在設計稿裡沒有獨立的側邊導覽項目（是從總覽/回測頁連結過去的），
// 這裡另外挑一個同風格的重疊圓圖示代表「比較」。
const ICONS: Record<PageId, ReactElement> = {
  strategies: (
    <>
      <path d="M12 3 L21 8 L12 13 L3 8 Z" />
      <path d="M3 13 L12 18 L21 13" />
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
      <div className="side-nav__items">
        {NAV_ITEMS.map((item) => (
          <button
            key={item.id}
            type="button"
            className={
              item.id === active ? "side-nav__item side-nav__item--active" : "side-nav__item"
            }
            aria-current={item.id === active ? "page" : undefined}
            onClick={() => onSelect(item.id)}
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
          </button>
        ))}
      </div>
    </nav>
  );
}
