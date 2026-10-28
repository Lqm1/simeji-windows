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
| `crates/transport`（simeji-transport） | ライブラリ | ユーザーのセッションごとの名前付きパイプ |

`assets/` は実行時アセット。`assets/native` のARMライブラリは `simeji-engine` に埋め込み、`assets/dictionary` はパッケージの `Dictionary` として同梱する。

## Windows側

`crates/ime` はWindowsに読み込まれるCOM/TSF DLL。変換エンジンそのものは保持しない。入力処理を、TSFに依存しない層と、TSF・COMとの接続層に分ける。

TSFに依存しない層:

- `keys`: 仮想キーと修飾キーを、IMEが扱うキー（変換、確定、取消、文節移動、候補番号など）へ対応付ける。
- `composer`: 未確定文字の状態機械。キー操作はMicrosoft IMEに合わせる。入力中は読みの中のカーソル（左側は入力中のローマ字を含み、右側はかな）を、変換中は文節ごとの読み・表記・選んだ候補を管理し、キーごとに文書へ反映する操作（表示、確定、取消、クラウド照会）を順に返す。確定時は選んだ文節の候補を前の文節の候補とともに学習し、何も選ばなければ文全体を学習する。
- `candidates`: エンジン候補とクラウド候補の統合、候補の選択、7件単位のページ。
- `worker`: 変換ワーカーへの要求。`composer` は `Converter` トレイトを通して変換を呼び出す。

TSF・COMとの接続層:

- `dll`: DLLエントリポイント、クラスファクトリ、アンロード判定。
- `text_service`: `ITfTextInputProcessorEx`、`ITfKeyEventSink`、`ITfCompartmentEventSink` などの実装。キーを `composer` に渡し、返された操作を編集セッションで文書へ適用する。確定時の学習とクラウド照会は、入力スコープが許す場合だけ行う。
- `input_mode`、`mode_icon`: 日本語入力のオン・オフをTSFのコンパートメント（`GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`）で管理する。アプリ・入力インジケーター・タッチキーボードからの変更にも従い、オフにすると入力中の文字を確定する。入力インジケーターには `GUID_LBI_INPUTMODE` のボタンとして「あ」「A」のアイコン（実行時に描画）を表示し、クリックで切り替える。有効化時は日本語入力をオンにする。
- `edit_session`: 未確定文字の書き込み、表示属性、キャレット、候補ウィンドウの位置。
- `candidate_window`: アプリのウィンドウに所有される独自クラスのウィンドウ。システムの配色で描き、選択中の候補を強調する。モニターのDPIに合わせて拡大し、表示・移動・非表示を `EVENT_OBJECT_IME_SHOW`・`EVENT_OBJECT_IME_CHANGE`・`EVENT_OBJECT_IME_HIDE` で通知する。クラス名にモジュールのアドレスを含め、別のアドレスで読み込まれた以前のDLLのクラスを使わない。
- `candidate_accessibility`: 候補ウィンドウのUIオートメーション。Microsoftの規約どおり、オートメーションID `IME_Candidate_Window` のリストと候補ごとのリスト項目（名前は候補、選択状態付き）を公開し、表示・非表示で `MenuOpened`・`MenuClosed`、選択の変更で `ElementSelected` イベントを発生させる。
- `display_attributes`、`input_scope`、`cloud_timer`、`registration`: 表示属性、入力スコープの判定、クラウド結果の取得、登録。

候補ウィンドウは2回目の変換操作で開く。最初の変換で文節候補を取得しないため、変換の応答が速い。

入力の状態をTSFに依存しない層に集めるのは、入力の応答、Backspace、候補操作を変更するときに、TSFや実機なしで挙動を単体テストできるようにするため。`composer` の状態はTSFやワーカーの呼び出し中に借用しないため、操作の適用中にTSFから再入（合成の終了通知など）されても競合しない。ワーカーの失敗でキー処理が途中で止まっても、それまでに返された操作（直前の確定など）は文書へ適用する。文書へ何も反映できなかった場合は、キーを処理する前の状態に戻す。

`simeji-setup` は登録・解除とDLLの検査を担当する。現在の `verify` はDLLの直接ロードとCOMインターフェースの検査であり、登録済みのIMEをWindowsが有効化して入力できることを保証するものではない。

## 変換ワーカー

`simeji-worker` が変換エンジンを保持する。`server` が要求をエンジンとクラウドに振り分け、ネイティブ実行が失敗した場合は次の要求の前にエンジンを開き直す。`simeji-launcher` はワーカーを起動し、`simeji-cloud` はクラウド設定を変更する。

`crates/engine` の `Engine` は変換・学習・辞書登録、`watchdog` はネイティブ呼び出しの時間制限、`user_dictionary` はユーザー辞書の保存、`arm` はARM実行、`elf` はライブラリのロードとリンク、`host` はライブラリが呼び出すAndroidのlibc関数（ヒープ、文字列、ファイル、書式、スレッド・時刻）の実装を担当する。

ネイティブ呼び出しの時間制限はUnicorn自身のタイムアウトを使わず、`watchdog` のスレッドが期限を過ぎた呼び出しだけを停止する。Unicornのタイムアウトは呼び出しごとにスレッドを作り、Windowsでは15.6msのタイマー刻みで待つため、呼び出し1回ごとに約15msかかっていた。

元のエンジンと辞書を使うのは、独自変換による近似を避けるため。ARMライブラリをUnicornで実行することで、Windows x64から利用する。TSF DLLとワーカーを分け、エンジンの実行状態を入力先アプリのプロセスから分離する。ただし、同期要求を待つ間の入力遅延まで防ぐ構成ではない。

`crates/protocol` は変換・学習などの要求と応答の型、`crates/transport` は名前付きパイプとタイムアウト付きの通信を担当する。

パイプは `\\.\pipe\SimejiWindows\<ユーザーSID>\<セッションID>\Worker`。所有者をユーザーのSIDに固定し、そのユーザーに全権限、同じユーザーのAppContainerプロセスに読み書きを許可し、低整合性レベルからの書き込みを許可する。クライアントは所有者が自分のユーザーであることを確認してから送信し、サーバーには識別だけを許して偽装を許さない。所有者を明示するため、ワーカーが管理者権限で動いていても通常権限のアプリから接続できる。

## クラウド

`crates/worker` の `cloud::CloudClient` は有効化設定、HTTP通信、候補の読み取り、キャッシュを扱う。`cloud::CloudQueue` はバックグラウンド要求と入力更新時の古い結果の排除を担当する。どちらも同じ関数で有効化設定を読み、内容が `true` のときだけ有効とする。TSF側は読みと応答の対応を確認して候補を追加する。HTTP処理を入力先アプリで実行しない。

## 配置と権限

`scripts/build.ps1` はローカル用パッケージを作る。インストーラーは管理者権限で実行し、プログラムと辞書を `%ProgramFiles%\SimejiWindows` に配置する。Program Files はAppContainerから読めるため、Microsoftの要件どおり辞書をここに置く。アプリが読み込んだままのDLLは上書きできないので、名前を変えて退避し、次回のインストールで削除する。

`simeji-setup register` はCOMクラスを `HKLM` に、TSFプロファイルとカテゴリをすべてのユーザー向けに登録し、`InstallLayoutOrTip` で実行したユーザーの入力方式に追加する。ユーザー単位のCOM登録はコンピューター全体の登録より優先されるため、登録・解除の両方で削除する。入力インジケーターのブランドアイコン（白い四角に黒い「S」）は、ビルドスクリプトがMicrosoftの指定する16〜48ピクセルで描いてDLLのリソースに含め、プロファイルはその最初のアイコンを指す。カテゴリはキーボード（`GUID_TFCAT_TIP_KEYBOARD`）と表示属性に加え、Windowsストアアプリ対応（`IMMERSIVESUPPORT`）、入力インジケーター対応（`SYSTRAYSUPPORT`）、入力モードのコンパートメント（`INPUTMODECOMPARTMENT`）、COMを介さない読み込み（`COMLESS`）を登録する。ワーカーの自動起動は `HKLM` の `Run` に登録する。

ユーザーデータは `%LOCALAPPDATA%\SimejiWindows` に置く。`settings.json`（設定）、`user-dictionary.json`（ユーザー辞書）、`Learning\`（エンジンの学習データ）、`Cache\`（削除可能なクラウドキャッシュ）に分ける。

ワーカーは入力先アプリと接続できる通常権限で起動する。インストールに管理者権限が必要なことと、常駐ワーカーを管理者として起動することは別である。

## 変更の境界

キー操作、削除・確定・取消、入力モードは主に `crates/ime` の `keys` と `composer`、候補の見た目は `candidate_window`、フォーカスは `text_service` で扱う。応答速度や再接続の改善は通信・ワーカーにも及びうる。変換コアを当面維持する方針は、これらの内部実装を固定するという意味ではない。
