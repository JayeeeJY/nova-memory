# 使用 Nova Memory 本地版

[English](GETTING_STARTED.en.md) · [简体中文](GETTING_STARTED.md) · 繁體中文 · [日本語](GETTING_STARTED.ja.md)

[產品介紹](../README.zh-Hant.md)

當前 GitHub 發布的是 **v0.1.0** 本地源碼預覽。建議先在 Apple 晶片 Mac 上自行構建。正式安裝包的開發者簽名、公證和換機檢查仍在準備。版本號、更新日誌或 GitHub Release 只標出這次源碼預覽，並不等於已經提供簽名安裝包，也不等於真實客戶端或設備驗收已經完成。當前桌面控制項是中文。下面的步驟是操作說明，不是一次已完成的 Mac 運行記錄。

## 構建並打開

準備 macOS 14 或更新版本、帶 macOS SDK 的 Xcode、Rust 1.97.1 和 Python 3.11 或更新版本。

```sh
git clone https://github.com/JayeeeJY/nova-memory.git
cd nova-memory
cargo fetch --locked
./script/build_and_run.sh
```

入口在新的 `.artifacts/` 目錄中構建運行包與桌面 App，然後打開候選。依賴版本由 `Cargo.lock` 固定。已構建的 App 攜帶本機運行程序。

想使用獨立測試庫，可以指定一個新的絕對路徑和空閒端口。

```sh
./script/build_and_run.sh \
  --profile-dir "/absolute/path/to/new-nova-profile" \
  --port 3060
```

構建需要下載依賴並使用 Xcode 的命令行工具。如果系統找不到編譯器，先在 Xcode 設置中確認命令行工具已安裝和選定。

## 第一次保存

1. 打開 App，點「开始本地使用」。
2. 在工作台點「留住一段记忆」，粘貼一小段適合保留的材料並保存。
3. 到「AI 整理」查看原文、接收記錄與處理狀態。
4. 準備好模型後，在運行設定中選擇模型、服務地址和處理方式，再開啟整理。

未配置模型時，原文會保留並等待整理。模型連接與提取品質需要分別核對，結果可以繼續修正。

## 在其他 AI 中使用

工作台「连接」按所選平臺提供接入步驟。

- 持續使用時，為目標 AI 選擇採集或讀取範圍，並安裝相應 MCP 配置。
- 臨時分享時，在「记忆库」選擇需要的記錄，預覽一次性上下文，再複製或下載到目標工具。
- 已發出的一次性文本由接收方保管。後續版本修改和連接撤銷作用於後續 Nova 訪問。

ChatGPT、Grok 等遠程連接器需要可達的服務地址、認證和相應賬號配置。具體方式以當前工作台給出的平臺說明為準。

## 原件、版本與修正

在「记忆库」查看項目或主題，展開來源及適用條件。到「AI 整理」告訴 Nova 哪一處不準確或只適用於什麼場景，系統保留修正的來源與歷史。

模型使用的材料及處理範圍由當前設置決定。連接遠程服務時，相應內容會發送至所配置的服務。

## 數據與備份

預設數據目錄為 `~/Library/Application Support/Nova Memory/local-workspace-v03`。停止服務後，材料和配置繼續保存在該目錄中。關閉 App 窗口後服務可能繼續運行，需要結束時使用 App 的「停止」。

工作台的設置與數據區域可以管理備份。加密備份檢查需要備份文件和配套的 Vault 密鑰；從桌面「运行设置 → 加密备份」選擇文件，核對並打開獨立副本。

檢查副本保持採集、自動整理與 AI 連接暫停。重新啟用前核對備份之後的刪除和撤權狀態。

## 開發驗證

```sh
python3 -m unittest discover -s scripts/tests -p 'test_*package.py'
python3 -m unittest discover -s scripts/tests -p 'test_desktop_local_signing.py'
```

腳本的 `--help` 可查看各自的打包參數。

```sh
python3 scripts/package-local.py --help
python3 scripts/package-desktop.py --help
python3 scripts/sign-desktop-local.py --help
```

參與改進前閱讀 [CONTRIBUTING.md](../CONTRIBUTING.md)。反饋中提供版本、系統、目標 AI 客戶端、最小合成復現步驟與預期結果。
