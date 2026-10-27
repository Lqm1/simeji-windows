# Simeji Windows

Windows 11 x64向けのRust製日本語IMEです。Simeji 23.6.1の変換エンジンと辞書を使い、ARMライブラリをUnicornで実行します。変換はワーカー、入力欄との連携はTSF DLLが担当します。

利用者によるChrome・Codexなどでの日本語入力確認は完了しています。現在は入力のもたつき、Backspace、変換候補画面などの改善段階です。確認の範囲と残課題は[検証状況](docs/status.md)を参照してください。

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

`%LOCALAPPDATA%\SimejiWindows\Versions` にプログラムを配置し、TSFプロファイルと現在のユーザーのログイン時起動を登録します。インストール先の `simeji-launcher.exe` を通常権限のセッションから起動してください。次のサインインからは自動起動します。Windowsの入力切替で「Simeji Windows」を選び、対象アプリを再起動して確認してください。

半角／全角で日本語入力を切り替えます。ローマ字入力後、Spaceで変換、Space・上下キーで候補選択、左右キーで文節移動、1～7で表示中の候補を確定、Enterで確定、Escapeで変換解除／キャンセルです。

## クラウド変換

初期状態はオフです。有効にすると、入力中の読みを `https://cloud.simeji.me/py` へ送ります。入力スコープがパスワード・PIN・プライベート指定、または確認に失敗した場合は送信と学習を抑止します。アプリ側が入力欄を正しく通知する必要があります。

インストールした版のフォルダーで次を実行します。

```powershell
.\simeji-cloud.exe "$env:LOCALAPPDATA\SimejiWindows\UserData" enable
.\simeji-cloud.exe "$env:LOCALAPPDATA\SimejiWindows\UserData" disable
```

候補は、変換開始前に現在の読みと一致する応答を取得できた場合に加わります。通信失敗時もローカル変換を継続します。候補の縦表示はWindows向けで、Androidの横並び・画像表示は再現対象に含みません。

## ユーザー辞書と解除

ワーカー起動中に `simeji-worker.exe add-word <読み> <単語>` で登録できます。学習と辞書は `%LOCALAPPDATA%\SimejiWindows\UserData` へ保存します。

インストール先の版の `uninstall.ps1` を管理者PowerShellで実行すると、IME登録とログイン時起動を解除します。学習・辞書・プログラムファイルは残します。

## 開発

所有権、モジュール分割、`Result`、テスト、FFIの扱いは[公式Rust Book](https://doc.rust-lang.org/book/)を基準にします。Windowsとの連携は[MicrosoftのTSF資料](https://learn.microsoft.com/en-us/windows/win32/tsf/text-services-framework)を参照します。

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

このリポジトリは個人利用・ローカルビルド向けです。公開用のバイナリ配布やリリースは作成しません。
