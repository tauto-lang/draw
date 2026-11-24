# wgpu

WGSL で書いた頂点・フラグメントシェーダを使い、矩形やメッシュをインスタンス描画する Tauto tote。macOS は Metal、Linux は Vulkan、Windows は DX12 を使う。描画の入口は `wgpu::render`。

## 画面外に描く

1. `Device::headless()` でデバイスを開く。通常のアダプターで失敗した場合はソフトウェアアダプターを要求し、両方失敗すると各試行の理由を返す。macOS のビルドは Metal のみで、ソフトウェア描画の利用は保証しない。
2. `Target::rgba8` で描画先を作り、`Pipeline::create` に描画先、WGSL、混色方式（`Replace` または `SourceOver`）を渡す。同じフォーマットの描画先ではパイプラインを再利用できる。
3. `Buffer::create` で `Storage` または `Uniform` のバッファを作り、`Buffer::write` でバイト列を転送する。オフセットと転送長は4バイト単位。整数マスクには `Texture::mask` と `Texture::write` を使う。RGBA8 のテクスチャとして使うときは `Target::texture` で同じ画像を参照できる。
4. `Pass::submit` に描画先、パイプライン、binding のリスト、クリア色（`0xRRGGBBAA`）、頂点数とインスタンス数を渡す。画面外では `clear_rgba: Some(色)` でクリアし、`None` で前のパスの内容を保持して描き重ねる。窓では submit ごとに新しいフレームを取得するため `Some(色)` を指定する。`None` を渡すとフレーム取得前に `Err` を返す。
5. `Target::read` で上端の左から順に並ぶ RGBA バイト列を得る。GPU のコピーで必要な行末の詰め物は bridge が取り除く。

[画面外描画の例](tests/src/scene.taut)は、インスタンスの寸法を storage buffer から、アルファマスクを R8Uint テクスチャから読む。頂点とフラグメントの入口は `vs_main` と `fs_main`。`Pass.bindings` の先頭が `@group(0) @binding(0)` に対応し、以降は順に番号を振る。バイト列のレイアウトは WGSL の宣言に合わせる。

リソースの番号はデバイスごとに検査する。各型の `close` で個別に解放でき、デバイスの破棄時には残りをまとめて解放する。`Target::texture` はターゲットと同じ資源を指すので、どちらかを閉じると両方の参照が失効する。検証エラーは wgpu の error scope から取得して `Err(Text)` で返す。scope 外から通知された場合は、未報告の先頭のエラーをデバイスに保持し、次の操作で返す。

## 窓に描く

Rust ホストが所有するウィンドウを、ウィンドウシステムのスレッド（macOS はメインスレッド）で bridge の `open_window` に渡す。引数は所有権を保持できる `SurfaceTarget<'static>` に変換できる値で、例えば対応するウィンドウの `Arc` を使う。戻り値はデバイスとウィンドウターゲットの番号。surface と互換のあるアダプターと表示フォーマットを選ぶ。

このデバイスを受け取った Tauto 側は `Target::window` で描画先を取得し、そのターゲットでパイプラインを作る。`Pass::submit` がフレームの取得、描画、present を行う。サイズ変更には `Target::resize_window` を使い、最小化などで幅か高さがゼロの間は描画を休む。ウィンドウターゲットの読み戻しとサンプリングは対象外。

窓では `Pass::submit_frame` で結果を分岐できる。`Submitted` は表示要求の完了、`TimedOut` はフレームを取得できず再試行可能、`Outdated` は現在の寸法での再設定が必要、`Lost` は surface の喪失を表す。再設定に失敗した後は、`Target::resize_window` が成功するまでフレームを取得しない。窓のフォーマットは RGBA8 または BGRA8 の Unorm に限定し、対応形式がなければ初期化時に理由を返す。これによりシェーダ出力への自動 sRGB 変換を避ける。

`Target::read` は mapping の完了を最大10秒待つ。テクスチャ書き込みで指定矩形に必要なバイト数が足りない場合は転送前に失敗する。

数値ハンドルだけを持つ窓の API からは、ウィンドウとディスプレイの生存期間を保証できない。そのため、この入口はホストから所有権つきウィンドウを受け取る。Tauto 側からは [winit](../winit) の `WinitWindow::take_device` で窓に対応する `Device` を受け取る。

## API の要点

| 操作 | この tote の API |
| --- | --- |
| デバイスと資源 | デバイスの作成は `Device::headless`。資源の作成・更新・解放は各型のモジュールに置く。backend 固有の番号、renderer 名、backend 選択は公開しない。 |
| シェーダ | 両ステージを一つの WGSL モジュールに書く。wgpu が全対象 backend に変換するので、MSL と GLSL ES の二重管理が要らない。 |
| 頂点とインスタンス | 頂点属性の記述を持たず、シェーダが vertex index、instance index と storage buffer からデータを読む。 |
| uniform とテクスチャ | 番号順のバッファ・テクスチャ binding で渡す。uniform の構造と詰め方は WGSL 側で定義する。 |
| 画像と読み戻し | R8Uint マスクと RGBA8 ターゲットを別のコンストラクターで作る。読み戻しは所有された RGBA バイト列。クリア色も RGBA 順。 |
| 混色とパス | 混色を列挙型で選び、描画先のフォーマットに合わせてパイプラインを作る。ウィンドウへの submit は present も行う。 |
| サイズと失敗 | 作成・更新・描画は `Result` を返す。画像サイズなどの上限はデバイスの制限に従う。 |

## 依存

Cargo 1.100.0 を `rust-toolchain.toml` で固定し、`.cargo/config.toml` で公開後14日未満の crate を解決しない（`global-min-publish-age = "14 days"`、`incompatible-publish-age = "deny"`）。bridge の lockfile は120パッケージで、外部119件はすべて crates.io から取得する。

wgpu 25.0.2 は既定 feature を無効にし、共通で `wgsl`、OS ごとに `metal`・`vulkan`・`dx12` の一つを追加する。pollster は 0.4.0 に固定。bridge には `#![forbid(unsafe_code)]` を指定している。

## 実行と確認

この tote のルートから実行する。

```sh
(cd bridge && cargo test --locked --offline)
tauto check src/render.taut
tauto check tests/src/main.taut
```

`cargo test` はデバイスを作るので、GPU のアダプターが見える環境が必要。アダプターが無いとアダプター取得の失敗で終了する。

画面外試験は `tests` で実行する。テスト tote の extern は実行環境で信頼済みであることが必要で、初回は Tauto が表示する信頼設定を確認する。

```sh
(cd tests && tauto run --backend rust .)
```

画面外試験は白地に赤と半透明の青を重ね、8×4 の全画素を期待値と比較する。アダプター取得失敗、描画失敗、画素不一致はいずれもエラー終了し、一致すると `32 RGBA pixels match` を出す。ウィンドウへの実表示は [winit](../winit) のテストで確認する。

## ライセンス

[MIT](LICENSE-MIT) と [Apache-2.0](LICENSE-APACHE) のデュアルライセンス。どちらかを選んで使える。
