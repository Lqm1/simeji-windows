# 現在の構成

製品の要求は[製品仕様](specification.md)、確認済みの範囲は[検証状況](status.md)を参照する。この文書は実装の責務と採用理由を説明する。

```text
入力先アプリ
  └─ TSF DLL（crates/ime）
       └─ 名前付きパイプ（crates/transport、crates/protocol）
            └─ simeji-worker（crates/worker）
                 ├─ Engine（crates/engine）→ Unicorn → ARM変換エンジン・辞書
                 ├─ 学習・ユーザー辞書
                 └─ CloudQueue → クラウド通信・キャッシュ
```

## クレート

Cargoワークスペースは、実行されるプロセスと責務ごとにクレートを分ける。

| クレート | 成果物 | 責務 |
| --- | --- | --- |
| `crates/ime`（simeji-ime） | `simeji_ime.dll`、`simeji-setup` | TSFテキストサービスと、その登録 |
| `crates/worker`（simeji-worker） | `simeji-worker`、`simeji-launcher`、`simeji-cloud` | 変換ワーカー、その起動、クラウド設定 |
| `crates/engine`（simeji-engine） | ライブラリ | ARM変換エンジンの実行と変換・学習API |
| `crates/protocol`（simeji-protocol） | ライブラリ | DLLとワーカー間の要求と応答の型 |
| `crates/transport`（simeji-transport） | ライブラリ | 所有者限定の名前付きパイプ |

`assets/` は実行時アセット。`assets/native` のARMライブラリは `simeji-engine` に埋め込み、`assets/dict` はパッケージに同梱する。

## Windows側

`crates/ime` はWindowsに読み込まれるCOM/TSF DLL。変換エンジンそのものは保持しない。入力処理を、TSFに依存しない層と、TSF・COMとの接続層に分ける。

TSFに依存しない層:

- `keys`: 仮想キーと修飾キーを、IMEが扱うキー（変換、確定、取消、文節移動、候補番号など）へ対応付ける。
- `composer`: 未確定文字の状態機械。ローマ字、読み、文節、候補選択、入力モードを管理し、キーごとに文書へ反映する操作（表示、確定、取消、クラウド照会）を順に返す。
- `candidates`: エンジン候補とクラウド候補の統合、候補の選択、7件単位のページ。
- `worker`: 変換ワーカーへの要求。`composer` は `Converter` トレイトを通して変換を呼び出す。

TSF・COMとの接続層:

- `server`: DLLエントリポイント、クラスファクトリ、アンロード判定。
- `text_service`: `ITfTextInputProcessor`、`ITfKeyEventSink` などの実装。キーを `composer` に渡し、返された操作を編集セッションで文書へ適用する。確定時の学習とクラウド照会は、入力スコープが許す場合だけ行う。
- `edit_session`: 未確定文字の書き込み、表示属性、キャレット、候補ウィンドウの位置。
- `display`、`candidate_window`、`input_scope`、`cloud_timer`、`registration`: 表示属性、候補ウィンドウ、入力スコープの判定、クラウド結果の取得、登録。

入力の状態をTSFに依存しない層に集めるのは、入力の応答、Backspace、候補操作を変更するときに、TSFや実機なしで挙動を単体テストできるようにするため。`composer` の状態はTSFやワーカーの呼び出し中に借用しないため、操作の適用中にTSFから再入（合成の終了通知など）されても競合しない。ワーカーの失敗でキー処理が途中で止まっても、それまでに返された操作（直前の確定など）は文書へ適用する。

`simeji-setup` は登録・解除とDLLの検査を担当する。現在の `verify` はDLLの直接ロードとCOMインターフェースの検査であり、登録済みのIMEをWindowsが有効化して入力できることを保証するものではない。

## 変換ワーカー

`simeji-worker` が変換エンジンを保持する。`server` が要求をエンジンとクラウドに振り分け、ネイティブ実行が失敗した場合は次の要求の前にエンジンを開き直す。`simeji-launcher` はワーカーを起動し、`simeji-cloud` はクラウド設定を変更する。

`crates/engine` の `Engine` は変換・学習・辞書登録、`user_dictionary` はユーザー辞書の保存、`arm` はARM実行、`elf` はライブラリのロード、`host` は必要なAndroid実行環境との接続を担当する。

元のエンジンと辞書を使うのは、独自変換による近似を避けるため。ARMライブラリをUnicornで実行することで、Windows x64から利用する。TSF DLLとワーカーを分け、エンジンの実行状態を入力先アプリのプロセスから分離する。ただし、同期要求を待つ間の入力遅延まで防ぐ構成ではない。

`crates/protocol` は変換・学習などの要求と応答の型、`crates/transport` は所有者に制限したローカル名前付きパイプとタイムアウト付きの通信を担当する。

## クラウド

`crates/worker` の `cloud::CloudClient` は有効化設定、HTTP通信、候補の読み取り、キャッシュを扱う。`cloud::CloudQueue` はバックグラウンド要求と入力更新時の古い結果の排除を担当する。どちらも同じ関数で有効化設定を読み、内容が `true` のときだけ有効とする。TSF側は読みと応答の対応を確認して候補を追加する。HTTP処理を入力先アプリで実行しない。

## 配置と権限

`scripts/build.ps1` はローカル用パッケージを作る。インストーラーは管理者権限で実行し、製品を `%LOCALAPPDATA%\SimejiWindows\Versions` 以下に配置して登録する。学習・ユーザー辞書・クラウド設定は `UserData` 以下に保存する。

ワーカーは入力先アプリと接続できる通常権限で起動する。インストールに管理者権限が必要なことと、常駐ワーカーを管理者として起動することは別である。

## 変更の境界

キー操作、削除・確定・取消、入力モードは主に `crates/ime` の `keys` と `composer`、候補の見た目は `candidate_window`、フォーカスは `text_service` で扱う。応答速度や再接続の改善は通信・ワーカーにも及びうる。変換コアを当面維持する方針は、これらの内部実装を固定するという意味ではない。
