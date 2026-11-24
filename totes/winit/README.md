# winit

Tauto の `winit::window` からデスクトップの窓を一つ開き、`wgpu::render` で描く tote。ウィンドウとイベントループを作ったスレッドで操作する。macOS はプロセスのメインスレッドを使う。

## 開いて描く

1. `WinitWindow::open(title, width, height)` で affine な窓を作る。初期サイズは正の物理画素数。`WinitWindow::open_fullscreen(title)` は今のモニターのボーダーレス全画面（winit の `Fullscreen::Borderless(None)`）で開く。大きさは `size` か `Resized` で受け取る。`set_fullscreen(&mut window, on)` で開いた後に全画面と通常の窓を切り替える。winit の resume コールバックで窓を作り、所有権を持つ `Arc<Window>` を wgpu bridge の `open_window` に渡す。
2. `take_device(&mut window)` でこの窓に対応する既存の wgpu tote の `Device` を一度だけ受け取る。`Target::window`、`Pipeline::create`、`Pass::submit_frame` は wgpu の API を使う。
3. `request_redraw` で再描画を要求する。`pull(&mut window, timeout_ms)` は待機中のイベントを一件返す。キューが空ならイベントループを pump する。0 はポーリング。指定時間内にイベントがなければ `Idle`。OS のコールバック自体が長引く場合もある。
4. `size` は現在のクライアント領域の幅と高さ（物理画素）、`scale` は論理画素に対する倍率の千分率（1500 は1.5倍）を返す。ゼロ寸法への resize は描画を休む。再表示時は正の寸法でターゲットを再設定する。
5. 描画資源とデバイスを破棄し、最後に `close(window)` で窓を消費する。surface も窓の所有権を持つ。winit のイベントループはプロセスにつき一度だけ生成する。

| Event | 値 |
| --- | --- |
| Idle / CloseRequested / RedrawRequested | 待機終了、閉じる要求、再描画の要求 |
| Resized / ScaleChanged | 新しい物理幅・高さ、または倍率の千分率 |
| FocusChanged | フォーカスの有無 |
| Key | 物理キー名、配列を反映した論理キー、押下か、リピートか |
| WheelLines / WheelPixels | 横・縦の符号付きスクロール量。各値は行または物理画素の千分率。例えば `WheelLines(0, -250)` は縦に −0.25 行。符号は winit の通知と同じ |
| PointerMoved / PointerLeft | クライアント領域での符号付き物理画素座標（最も近い整数）、または領域からの退出 |
| PointerButton | ボタン名と押下かどうか |

キーを照合するとき、物理位置には `Code(KeyA)` や `Code(ArrowDown)`、文字入力には論理キーの `Character(Text)` を使う。例えば同じ `Code(KeyA)` が、配列によって `Character("a")` や `Character("q")` になる。論理キーはほかに `Named("Enter")`、`Dead(Text)`（アクセント不明時は空文字）、`UnknownKey` がある。これはキーイベントであり、IME の確定文字列は扱わない。

物理キー名と named key の綴りは [bridge の固定一覧](bridge/src/keys.rs) に従う。物理キーのネイティブコードは `MacOS(数字)`・`Windows(数字)`・`Xkb(数字)`・`Android(数字)` の十進表記、コード不明時は `Unidentified`。旧版の `Key(物理, 押下, リピート)` を照合していた箇所は、第2引数に論理キー（不要なら `_`）を追加する。

## 起動スレッドと pump

対象は macOS・Windows・Linux（X11、Wayland）。Linux と Windows は winit の `with_any_thread(true)` で作成スレッドをループの所有者にする。macOS では `objc2_foundation::MainThreadMarker::new()` で AppKit のメインスレッドかを判定し、違えばイベントループの生成前に `Err` を返す。bridge には `#![forbid(unsafe_code)]` を指定している。

イベントループと窓は、開いたスレッドの `thread_local!` に置き、そのスレッドが所有する。Tauto に渡す `WinitWindow` は番号と所有スレッドの ID と、まだ渡していない wgpu の `Device` だけを持つハンドルで、`Send + Sync`。そのため task を起こすプログラム（Rust backend が extern の型に `Send + Sync` を求める）でも使える。所有スレッド以外からの呼び出しは panic せず `Err("winit window must be called on the thread that opened the window")` を返す。`close` は窓を隠し、そのスレッドの状態を破棄する。

確認した Tauto 0.1.0（SHA-256 先頭 `e137d45bf915c5f7`）の通常の Rust 起動経路は `run_main_baked` からスタックを確保したワーカースレッドでプログラムを実行する。macOS のテストには公開 API `tauto_rt::run_embedded` を使い、同じ Tauto プログラムをプロセスのメインスレッドから実行する小さな Rust ホストを付けている。埋め込みホストは Tauto の権限を既定値で初期化する。

winit 0.30.12 の `pump_app_events` は Windows・Linux・macOS・Android が対応し、Web と iOS は非対応。Android のライフサイクルはこの tote の対象外。macOS は pump の外で NSApplication のループが止まる。Tauto に返る再描画イベントは非同期の通知であり、AppKit の drawRect コールバック内の描画は保証できない。ライブリサイズ中に内容の更新が止まる、または表示が乱れることがある。[winit の pump API と制約](https://docs.rs/winit/0.30.12/winit/platform/pump_events/trait.EventLoopExtPumpEvents.html)

## 実行と確認

この tote のルートから実行する。テスト tote の extern 信頼対象は winit と wgpu。この tote の `Tauto.eure` も、依存する wgpu の extern を `trust { extern = ["wgpu"] }` で信頼する。

```sh
(cd bridge && cargo test --locked --offline)
tauto check tests/src/main.taut
```

Linux・Windows の Rust backend では `tests` で `tauto run --backend rust .` を実行する。macOS は以下の手順。初回は Tauto が表示するテスト tote の信頼設定を確認する。最初に通常の Rust backend で tote をビルドし、生成された crate の example としてメインスレッド用ホストを起動する。

```sh
cd tests
tauto build --backend rust .
mkdir -p target/tauto/ws/winit-example/examples
cp main_thread.rs target/tauto/ws/winit-example/examples/main_thread.rs
cd target/tauto/ws/winit-example
cargo run --locked --offline --example main_thread
```

期待する表示は、タイトルが「Tauto winit」の640×400物理画素の窓。暗い背景の中央に橙色の四角形が描かれる。テストは resize と再描画を処理し、短時間後に窓を閉じる。present の成功を一度以上確認すると `rectangle presented` を出す。画面なしの単体テストはイベント変換、倍率、macOS の誤スレッド拒否、キー名と論理キー、ホイールの単位と端数、全画面と通常の窓の属性、ハンドルとイベントが `Send + Sync` であること、別スレッドからの呼び出しが `Err` になることの8件。macOS で8件とも成功し、通常の Tauto 起動は `winit requires the macOS process main thread` で終了した。メインスレッド用ホストは実機で `rectangle presented`、終了コード0。画面のピクセル採取、ライブリサイズの目視、Windows・Linux の実機確認は未実施。

## 依存

Cargo 1.100.0、公開待機14日、resolver は deny。winit 0.30.12 は既定 feature を切り、共通で `rwh_06`、Linux で `x11`・`wayland`・`wayland-dlopen` を使う。macOS のスレッド確認には winit と共通の objc2-foundation 0.2.2 を使い、直接指定する feature は `NSThread` のみ（既定 feature 無効）。objc2 は0.5.3の一つ。

wgpu は同じ repository の `totes/wgpu` にあり、tote は `../wgpu`、bridge は `../../wgpu/bridge`、テストは `../../wgpu` の相対 path で参照する。同じ commit の内容を一緒にビルドする。

bridge の lockfile は235パッケージ（registry 233）。wgpu bridge の120パッケージから **純増115**。テストは runtime を含め326パッケージ（registry 320）。どちらの lockfile にも wgpu は25.0.2の一つ、raw-window-handle は0.6.2の一つだけ。両方の全 registry エントリで crates.io の取得元・index の checksum・公開日時を照合し、2026-11-22 時点で公開後14日以上だった。最も新しい日時は2026-11-06 20:27:25 UTC。

## ライセンス

[MIT](LICENSE-MIT) と [Apache-2.0](LICENSE-APACHE) のデュアルライセンス。
