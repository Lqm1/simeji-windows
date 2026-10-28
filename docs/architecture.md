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

- `keys`: 仮想キーと修飾キーを、IMEが扱うキー（変換、確定、取消、文節移動、候補番号など）へ対応付ける。タッチキーボードが `VK_PACKET` で送る文字のうち、候補のページキー（U+F003、U+F004）は文字と区別する。
- `composer`: 未確定文字の状態機械。キー操作はMicrosoft IMEに合わせる。入力中は読みの中のカーソル（左側は入力中のローマ字を含み、右側はかな）を、変換中は文節ごとの読み・表記・選んだ候補を管理し、キーごとに文書へ反映する操作（表示、確定、取消、クラウド照会）を順に返す。確定時は選んだ文節の候補を前の文節の候補とともに学習し、何も選ばなければ文全体を学習する。
- `candidates`: エンジン候補とクラウド候補の統合、候補の選択、9件単位のページ。
- `worker`: 変換ワーカーへの要求。`composer` は `Converter` トレイトを通して変換を呼び出す。

TSF・COMとの接続層:

- `dll`: DLLエントリポイント、クラスファクトリ、アンロード判定。
- `text_service`: `ITfTextInputProcessorEx`、`ITfKeyEventSink`、`ITfCompartmentEventSink` などの実装。キーを `composer` に渡し、返された操作を編集セッションで文書へ適用する。確定時の学習とクラウド照会は、入力スコープが許す場合だけ行う。入力スコープは、Microsoftの資料どおりアプリのプロパティ（`ITfContext::GetAppProperty`）として読む。
- `input_mode`、`mode_icon`: 日本語入力のオン・オフをTSFのコンパートメント（`GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`）で管理する。アプリ・入力インジケーター・タッチキーボードからの変更にも従い、オフにすると入力中の文字を確定する。入力インジケーターには `GUID_LBI_INPUTMODE` のボタンとして「あ」「A」のアイコン（メイリオの通常ウェイトで実行時に描画）を表示し、クリックで切り替える。有効化時は日本語入力をオンにする。文字は4倍の解像度で描き、フォントの行高ではなく文字の輪郭を基準に余白1ピクセルで中央配置して縮小する。白い文字と半透明の黒い輪郭で、曲線と縁の濃さを滑らかにする。フォントファイルの同梱は不要。
- `edit_session`: 未確定文字の書き込み、表示属性、キャレット、候補ウィンドウの位置。
- `candidate_window`: アプリのウィンドウに所有される独自クラスのウィンドウ。標準Microsoft IMEを参考に、最大9件の番号付き一覧、灰色の選択背景、左端のマーク、下部のページ操作を描く。操作案内のヘッダーは置かない。Win32/GDIで描画し、DWMの角丸と枠色、Windowsのアプリのライト／ダーク設定、ハイコントラストのシステム色を使う。通常のTSF候補UIにはMicrosoft IMEの画面をそのまま呼び出すAPIがないため、このウィンドウを用いる。所有するアプリのウィンドウのDPIに合わせて拡大する。未確定文字の下に開き、モニターの作業領域からはみ出す場合や、タッチキーボード（`IFrameworkInputPane` の位置）に重なる場合は上に開く。表示・移動・非表示を `EVENT_OBJECT_IME_SHOW`・`EVENT_OBJECT_IME_CHANGE`・`EVENT_OBJECT_IME_HIDE` で通知する。クラス名にモジュールのアドレスを含め、別のアドレスで読み込まれた以前のDLLのクラスを使わない。
- `candidate_accessibility`: 候補ウィンドウのUIオートメーション。Microsoftの規約どおり、オートメーションID `IME_Candidate_Window` のリストと候補ごとのリスト項目（名前は候補、選択状態付き）を公開し、表示・非表示で `MenuOpened`・`MenuClosed`、選択の変更で `ElementSelected` イベントを発生させる。
- `candidate_element`: 候補一覧のTSF UI要素（`ITfCandidateListUIElementBehavior`、`ITfIntegratableCandidateListUIElement`）。候補の表示・変更・終了を `ITfUIElementMgr` に通知する。全画面のゲームや検索ボックスのように候補を自分で描くアプリ（UIレスモード）が非表示を求めた場合は、候補ウィンドウを出さない。アプリからの候補の選択・確定・取消は、キー操作と同じく `composer` を通して文書へ反映する。
- `functions`: TSFの関数プロバイダー。タッチキーボードには日本語のタッチ最適化レイアウト（`TKBL_OPT_JAPANESE_ABC`）を求める。検索（`ITfFnSearchCandidateProvider`）には、1語として変換される読みならその候補一覧を、文になる読みなら変換した文を返し、選ばれた候補を学習する。Microsoftの検索候補の条件どおり、読み全体の変換だけを、重複と、他の候補を先頭に含むものを除いて返す。
- `display_attributes`、`input_scope`、`cloud_timer`、`registration`: 表示属性、入力スコープの判定、クラウド結果の取得、登録。

候補ウィンドウは入力中から開く。`text_service` は読みの変更に対し既存の文変換APIでローカル候補を取得し、`composer` が読みを保ったまま候補を表示する。Tab・矢印・クリックで候補を選ぶと、その表記を未確定文字へ反映する。Spaceの1回目で文節変換と文節候補へ切り替える。現在は同期要求を使うため、入力ごとに変換処理の待ち時間が加わる。予測処理の非同期化やエンジンの最適化は別の作業とする。

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

`crates/worker` の `cloud::CloudClient` は有効化設定、HTTP通信、候補の読み取り、キャッシュを扱う。`cloud::CloudQueue` はバックグラウンド要求と入力更新時の古い結果の排除を担当する。どちらも同じ関数で有効化設定を読み、内容が `true` のときだけ有効とする。TSF側は読みと応答の対応を確認して候補を追加する。`cloud_timer` は変換開始後も結果を取得し、編集セッションで候補UIを更新する。後着結果はローカル候補の後ろに追加し、既存の選択位置を維持する。候補の由来は文字列と別に保持し、雲マークは確定文字に含めない。UIAでもクラウド候補と選択開始前の状態を区別する。HTTP処理を入力先アプリで実行しない。

## 配置と権限

`scripts/build.ps1` はローカル用パッケージを作る。インストーラーは管理者権限で実行し、プログラムと辞書を `%ProgramFiles%\SimejiWindows` に配置する。Program Files はAppContainerから読めるため、Microsoftの要件どおり辞書をここに置く。アプリが読み込んだままのDLLなど使用中のファイルは、`simeji-setup remove` が名前を変えて退避し、Windowsの再起動時に削除されるよう登録する。元の名前のまま登録すると、その後に再インストールしたファイルが再起動時に削除されるため。アンインストールと以前の `Versions` の削除も同じ方法を使う。

`simeji-setup register` はCOMクラスを `HKLM` に、TSFプロファイルとカテゴリをすべてのユーザー向けに登録し、`InstallLayoutOrTip` で実行したユーザーの入力方式に追加する。ユーザー単位のCOM登録はコンピューター全体の登録より優先されるため、登録・解除の両方で削除する。入力インジケーターのブランドアイコンは、利用者の指定に合わせSimejiアプリと同じロゴ（`assets/branding/simeji.png`）を使う。これはMicrosoftの白黒ブランドアイコンの指針よりアプリとの同一性を優先した表示である。16・20・24・32・40・48・64・128・256ピクセルを含む `simeji.ico` をビルド時にDLLのリソースへ埋め込み、プロファイルはその最初のアイコンを指す。カテゴリはキーボード（`GUID_TFCAT_TIP_KEYBOARD`）と表示属性に加え、Windowsストアアプリ対応（`IMMERSIVESUPPORT`）、入力インジケーター対応（`SYSTRAYSUPPORT`）、入力モードのコンパートメント（`INPUTMODECOMPARTMENT`）、COMを介さない読み込み（`COMLESS`）、UIレスモード（`UIELEMENTENABLED`）を登録する。ワーカーの自動起動は `HKLM` の `Run` に登録する。

`install.ps1` は `migrate-user-data.ps1` で、以前のユーザー単位のインストール（`UserData` と `Versions`）を新しい配置へ移す。

ユーザーデータは `%LOCALAPPDATA%\SimejiWindows` に置く。`settings.json`（設定）、`user-dictionary.json`（ユーザー辞書）、`Learning\`（エンジンの学習データ）、`Cache\`（削除可能なクラウドキャッシュ）に分ける。

ワーカーは入力先アプリと接続できる通常権限で起動する。インストールに管理者権限が必要なことと、常駐ワーカーを管理者として起動することは別である。

## 変更の境界

キー操作、削除・確定・取消、入力モードは主に `crates/ime` の `keys` と `composer`、候補の見た目は `candidate_window`、フォーカスは `text_service` で扱う。応答速度や再接続の改善は通信・ワーカーにも及びうる。変換コアを当面維持する方針は、これらの内部実装を固定するという意味ではない。
