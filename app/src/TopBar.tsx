interface TopBarProps {
  title: string;
}

// 設計稿 TopBar.dc.html 還畫了模式徽章、帳戶權益、今日損益、「全部停止」等即時
// 交易資訊——這個 App 現在只有回測引擎、沒有帳戶/持倉資料，硬做那些欄位只會是
// 假數字或假按鈕（違反公司規則「未實作功能」），所以先只做標題列，之後接上
// 模擬/實盤帳戶狀態時再補。
export function TopBar({ title }: TopBarProps) {
  return (
    <header className="top-bar">
      <h1>{title}</h1>
    </header>
  );
}
