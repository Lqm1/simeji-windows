# Simeji Windows

Windows 11 x64向けのRust製日本語IMEです。Simeji 23.6.1の変換エンジンと辞書を使い、ARMライブラリをUnicornで実行します。変換はワーカー、入力欄との連携はTSF DLLが担当します。

キー操作はMicrosoft IMEに、インストール・登録・データの配置はWindows IMEの標準的な形に合わせています。確認の範囲と残課題は[検証状況](docs/status.md)を参照してください。

## 製品文書

- [製品仕様](docs/specification.md): 合意した目的、対象範囲、変換・クラウドの要件、開発方針。
- [現在の構成](docs/architecture.md): TSF、ワーカー、変換エンジン、通信、配置の責務。
- [検証状況と残課題](docs/status.md): 確認済みの範囲、発生した問題、今後の改善。

## ビルド

Rust 1.99.1、Visual Studio C++ Build Tools、Windows SDK、CMake、Pythonを用意します。

```powershell
python -m pip install libclang ninja
.\scripts\build.ps1
```

パッケージとCargoの出力は既定で一時ディレクトリへ作成します。`CARGO_TARGET_DIR`、`LIBCLANG_PATH`、`-OutputDirectory`で変更できます。解析データはビルドに不要です。

## ローカル登録

ビルド結果のパッケージ内にある `install.ps1` を、同じWindowsユーザーの管理者PowerShellから実行します。

```powershell
& '生成されたパッケージのパス\install.ps1'
```

`%ProgramFiles%\SimejiWindows` にプログラムと辞書を配置し、IMEをすべてのユーザー向けに登録して、実行したユーザーの入力方式に追加します。ワーカーはサインイン時に自動起動します。インストール直後は `%ProgramFiles%\SimejiWindows\simeji-launcher.exe` を通常権限のセッションから起動するか、サインインし直してください。Windowsの入力切替で「Simeji Windows」を選び、開いていたアプリを再起動して確認してください。

キー操作はMicrosoft IMEに合わせています。日本語入力のオン・オフはタスクバーの入力インジケーター（「あ」「A」）にも表示され、クリックでも切り替えられます。

| 状態 | キー | 動作 |
| --- | --- | --- |
| 共通 | 半角／全角、漢字 | 日本語入力のオン・オフ（入力中の文字は確定） |
| 入力中 | Space | 変換 |
| 入力中 | Enter | 確定 |
| 入力中 | Escape | 入力の取り消し |
| 入力中 | Backspace／Delete | カーソルの前／後ろの1文字を削除 |
| 入力中 | ←→、Home、End（↓） | カーソル移動 |
| 変換中 | Space／↓、Shift+Space／↑ | 次／前の候補。2回目から候補ウィンドウを表示 |
| 変換中 | Shift+↓／Shift+↑ | 候補の次／前のページ |
| 変換中 | 1～9 | 候補ウィンドウの番号の候補で確定 |
| 変換中 | ←→、Home、End | 文節の移動 |
| 変換中 | Shift+←→ | 文節の長さを変更 |
| 変換中 | Escape、Backspace、Delete | 変換をやめて読みに戻る |
| 変換中 | Enter／文字の入力 | 確定（文字の入力はその後に続けて入力） |

入力の末尾の「n」は、変換・確定・カーソル移動のときに「ん」になります。

## クラウド変換

初期状態はオフです。有効にすると、入力中の読みを `https://cloud.simeji.me/py` へ送ります。入力スコープがパスワード・PIN・プライベート指定、または確認に失敗した場合は送信と学習を抑止します。アプリ側が入力欄を正しく通知する必要があります。

次のコマンドで切り替えます。設定は `%LOCALAPPDATA%\SimejiWindows\settings.json` に保存します。

```powershell
& "$env:ProgramFiles\SimejiWindows\simeji-cloud.exe" enable
& "$env:ProgramFiles\SimejiWindows\simeji-cloud.exe" disable
```

候補は、変換開始前に現在の読みと一致する応答を取得できた場合に加わります。通信失敗時もローカル変換を継続します。候補の縦表示はWindows向けで、Androidの横並び・画像表示は再現対象に含みません。

## ユーザー辞書と解除

ワーカー起動中に `simeji-worker.exe add-word <読み> <単語>` で登録できます。データはユーザーごとに `%LOCALAPPDATA%\SimejiWindows` へ保存します。

| 場所 | 内容 |
| --- | --- |
| `settings.json` | 設定（クラウド変換の有効・無効） |
| `user-dictionary.json` | ユーザー辞書 |
| `Learning\` | 変換エンジンの学習データ |
| `Cache\` | クラウド変換のキャッシュ。削除しても構いません |

`%ProgramFiles%\SimejiWindows\uninstall.ps1` を管理者PowerShellで実行すると、IME登録と自動起動を解除し、プログラムを削除します。ユーザーデータは残します。

## 開発

ソースは `crates/` 以下のCargoワークスペースです。TSF DLL（`crates/ime`）、変換ワーカー（`crates/worker`）、変換エンジン（`crates/engine`）、通信（`crates/protocol`、`crates/transport`）の責務は[現在の構成](docs/architecture.md)を参照してください。

所有権、モジュール分割、`Result`、テスト、FFIの扱いは[公式Rust Book](https://doc.rust-lang.org/book/)を基準にします。Windowsとの連携は[MicrosoftのTSF資料](https://learn.microsoft.com/en-us/windows/win32/tsf/text-services-framework)を参照します。

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

このリポジトリは個人利用・ローカルビルド向けです。公開用のバイナリ配布やリリースは作成しません。
