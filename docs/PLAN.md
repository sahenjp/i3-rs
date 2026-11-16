# i3wm → Rust 完全1:1移植プラン

対象: i3 4.25 (commit 8a97b0f, 2026-11-01)
方針: **機能を1つも追加しない**。IPC/X11プロトコル・JSON・エラー文言・ログ形式に至るまで
C版と同じ振る舞いをする1:1ポート。上流の「実装」だけをRustに置き換える。

---

## 0. 規模の実感

| 部位 | C行数 | 内容 |
|---|---|---|
| src/*.c (WM本体) | 28,555 | ツリー/レイアウト/IPC/設定/X11層 46ファイル |
| include/*.h | 6,936 | data.h(Con)等 |
| i3bar | 5,307 | バー本体+tray+子プロセス管理 |
| 周辺Cバイナリ | ~6,488 | i3-msg/nagbar/input/dump-log/config-wizard |
| libi3 | 2,775 | 共通ライブラリ(29ファイル) |
| Perl/sh(移植不要) | ~1,300 | save-tree, dmenu-desktop, migrate, sensible-* |
| テストスイート | 285テスト | Perl TAP, Xephyr+Xvfbベース |

C実装 合計 約4.4万行 → Rust 換算 約4〜5万行。spec駆動パーサは
commands.spec(554行)/config.spec(117状態・93コール)からコード生成で復元。

**結論: 技術的には完全に移植可能。障害は規模で、構造ではない。**
理由: (a) テストスイートがWMをブラックボックス(X11+IPC)として扱い、C版とRust版
の双方にそのまま走る (b) X11はプロトコル仕様で凍結されており両実装が同じ答えを返せる
(c) 非決定的部分はseed(乱数)・時刻・PIDのみで正規化比較可能。

---

## 1. クレート構成(cargo workspace)

```
i3-rs/
├── crates/
│   ├── libi3/        # libi3相当: IPCクライアント, i3String, font, draw_util, dpi, shmlog
│   ├── i3-spec/      # parser-specs/*.spec から状態機械を生成(build.rs) + 手書きハンドラ
│   ├── i3-core/      # WM本体: tree, con, workspace, floating, render, gaps, resize,
│   │                 #   handlers, ipc-server, config, bindings, match, manage, x(X11層)
│   ├── i3-atom/      # xmacroアトム一覧の生成(intern一括)
│   └── bins/         # i3, i3bar, i3-msg, i3-nagbar, i3-input, i3-dump-log, i3-config-wizard
└── testcases/        # 元i3のPerlスイートを無変更で流用(シムがi3バイナリを差し替えるだけ)
```

Rust側バイナリ名はそのまま i3 / i3bar / i3-msg / ... とし、パスとCLIフラグ
(-c -C -a -L -V -d --force-xinerama --disable-signalhandler --shmlog-size --restart 等)
を完全互換にする。`_I3_RESTART_FD` のexec越えも再現する。

## 2. 技術選定(決定事項)

| C | Rust | 理由 |
|---|---|---|
| libxcb + 補助ライブラリ | **x11rb**(同期API) | 純Rust実装・拡張(randr1.5/xkb/shape/xinerama/shm)生成済み。Cookie+discard_replyがi3の並列GetProperty/破棄パターン(manage.c)に直接対応 |
| libev | **calloop** | fd/シグナル/timer/pidfd。ev_prepare相当の「sleep前Xドレイン」はread-readyコールバック内でpoll+flushに等価変換 |
| yajl | **serde_json** | GET_TREE/イベントのJSONを型付きstruct+to_writer。緩い解析の互換は受け入れ側実装で吸収 |
| pcre2 | **pcre2クレート** | 後方参照・先読み・UCP付き\d等、ユーザ設定の正規表現互換維持にregexクレートは不可 |
| 侵入的TAILQ×4 | **slotmap + ConId** | Con1つがnodes/focus/floating/all_consの4リストに同時所属・parent逆ポインタ・close中のコールバック連鎖があり、Box/Rcツリーは借用規則と全面衝突。ID設計なら全て自然に解決 |
| cairo-xcb + pango | cairo-sys FFI手書き(3箇所) + pango/pangocairoクレート | cairo-rsにXCBサーフェスAPIが無いため、draw_util相当(333行)のみunsafe接着。DPI/論理pxをピクセル一致させる |
| shmlog(mmapリング) | memmap2 + rustix(shm_open) | 16Bヘッダ(offset_next_write/last_wrap/size/wrap_count)をABI互換のまま維持 → i3-dump-logがそのまま読める |
| generate-command-parser.pl | **build.rs内のRust移植** | specファイルを単一情報源として維持。生成物は状態enum+遷移テーブル+call dispatcher |
| wordexp(include) | globクレート+手展開 | include ~/*.conf の互換 |

## 3. データモデル(心臓部)

```rust
pub struct Tree {
    cons: SlotMap<ConId, Con>,
    croot: ConId,
    focused: ConId,
}
pub struct Con {
    type_: ConType,            // ROOT/OUTPUT/CON/FLOATING_CON/WORKSPACE/DOCKAREA
    parent: Option<ConId>,
    nodes: VecDeque<ConId>,     // タイリング順序(TAILQ nodes_head相当)
    floating: VecDeque<ConId>,  // floating_head相当
    focus_stack: VecDeque<ConId>, // focus_head相当, front=最近
    swallows: Vec<Match>,       // swallow_head相当
    marks: Vec<String>,
    window: Option<WindowId>,   // 別slotmap
    rect/window_rect/deco_rect/geometry: Rect, // ×4の矩形
    percent: f64, fullscreen_mode, layout, border_style, floating: FloatingState,
    urgency_timer: Option<TimerId>, // libevタイマー→calloopのtoken
    // 以下 data.h の全フィールドを1:1で
}
```

- on_remove_childの関数ポインタは実装が1つ(con_on_remove_child)なので直接呼び出しに置換。
- all_consの線形探索(con_by_window_id等)はslotmap走査+インデックス付与で同等以上の速度に。
- X依存(x_con_init/x_con_kill)はトレイト境界に切り出し、コア層をXなしで単体テスト可能に。

## 4. フェーズ計画(各フェーズ完了=動く成果物+検証)

### Phase 0: 足場(週1)
- 元i3をmesonでビルドし、285テストをこの環境でgreen化(Xephyr+Xvfbを導入)
- Rust 1.74→最新化(エディション・クレート要件)、cargo workspace雛形
- spec→build.rsコード生成のプロトタイプ(commands.specが通るか)
- **検証: complete-run.pl が C版で完走(ベースライン録画)**

### Phase 1: libi3相当 + i3-msg(週1) — 試金石
- IPCクライアント(magic "i3-ipc"+len+typeネイティブエンディアン)、ソケット探索
- i3-msgをRustで再現(-t/-m/-s/-q/-h/-v、JSON pretty、エラー文言)
- **検証: 同一i3に対しC版i3-msgとRust版の出力バイト一致(diff)**

### Phase 2: コアツリー(純粋ロジック、週2)
- data.h型群→Rust型、Con/Tree、con_new/attach/detach/focus/next_focused/
  fix_percent、tree_init(__i3擬似output含む)、workspace_get/show、tree_open_con/
  tree_split/tree_flatten/tree_close_internal、move.c、render.c(矩形計算のみ)、gaps.c
- **検証: 不変条件の単体テスト(フォーカスタック・percent和=1.0・ws num順・
  fullscreen唯一性)。X無しでrustdocコメント付き1:1関数対応表**

### Phase 3: X11層(週3) — 最小WMがXephyrで立つ
- x11rb接続・atoms一括intern・WM_Sn獲得競合15秒待ち・xkb(select_events+per_client_flags)・
  keysyms/load_keymap/grab_all_keys・randr(xinerama/fake_outputs含む)・イベントディスパッチ
  (handlers.cの全ハンドラ)・manage_window(manage.c)・x_con_init/kill・x_push_changes・
  ignore_eventのfull sequence機構(x11rbで自前補正)
- **検証: Xephyr手動起動、xterm配置・フォーカス移動・mod1+j/kでツリー操作。
  i3-nagbarなし設定エラー表示はPhase 5まで未実装で可**

### Phase 4: 設定パーサ(週2)
- 字句(リテラルstrncasecmp/number/string/word/end)・117状態遷移・
  変数展開(strcasestr互換+行番号保存)・include(glob)・エラー報告(^下線+前後2行)・
  93個のCFGFUNハンドラ・reload経路
- **検証: 元テストの201-config-parser系・バリエーションconfig一括パースで
  C版とエラー文言・行番号一致。5xx系も一部**

### Phase 5: コマンドパーサ+IPC全実装(週3)
- commands.spec生成状態機械・criteria・50コマンドハンドラ・
  IPC(13種GET/RUN/SUBSCRIBE/7イベント)・dump.c相当のget_tree JSON完全互換
- **検証: ここでPerlスイートの大部分(1xx/2xx系)がRust版で走る。
  失敗はテスト側で許容リスト化し収束追跡**

### Phase 6: 完装(週4)
- floating・resize・scratchpad・tiling_drag・load_layout/restore_layout(swallow)・
  urgencyタイマー・startup-notification・in-place restart(_I3_RESTART_FD)・
  shmlog ABI・sighandler・sd_notify・表示系キャッシュ最適化
- **検証: --restart モードのrestart-state.golden突合、185-190系、192-216系**

### Phase 7: i3bar + 周辺(週4)
- i3bar(子プロセスSIGSTOP/SIGCONT、i3bar-protocolヘッダ判定/無限配列クリック、
  XEmbed tray、短縮切替)・i3-nagbar・i3-input・i3-dump-log・i3-config-wizard
- **検証: i3status実プロセスとの往復、trayに実際のアプリ(確認できる範囲)、207-shmlog**

### Phase 8: 等価性締め(週2+)
- 285テスト全green(317-i3-dmenu-desktop等のPerl資産はC版共通でそのまま)
- golden差分: get_tree JSONとイベント列をC/Rust両版から録画し、
  con_id/window id/timestamp/rectを正規化してdiff
- cargo-fuzz: 設定パーサ・コマンドパーサ(201のエラーケースを期待値に回帰)
- ASan相当としてmiri対象外だが、debug_assert!(不変条件)を恒常化
- **検証: complete-run.pl --restart がRust版で完走。これが完成の定義**

## 5. 難所トップ(全領域統合)と対処

1. **tree_close_internal**(close連鎖+WM_DELETE中断+フォーカス先行計算)
   → 状態遷移順序を関数単位でアルゴリズムコメントごと1:1写経し、
     単体テストは「どの子から消えるか」の系列assert
2. **GET_TREE/JSON完全互換**(dump.c ~700行) → フィールド順序まで保持する
   シリアライズ手書き+スナップショットテスト(C版出力をexpectedファイル化)
3. **cairo-xcb FFI**(draw_util/font) → unsafeはこの3接点に局所化、
   ラッパはsafe API、DPIと論理pxのピクセル一致を明示テスト
4. **x_push_changesのスタック差分** → CIRCLEQの状態スタックをVecで再現、
   順序バグ=点滅を検出する「Z順序録画テスト」をXvfbで
5. **in-place restart**(fd越え+レイアウト保存) → OwnedFdのexec受け渡しを
   rustixで再現、単体ではできないのでPhase 6の統合テストに回す
6. **パーサの文言互換**(エラーJSON・nagbar行) → 期待値ファイルをC版から生成
7. **dragのモーダルループ** → calloopの一時停止+再帰的drainに変形、
   イベントリプレイ順序を維持
8. **i3barのtray手術** → XEmbed仕様部分を独立モジュール化し、
   リアルアプリ(VLCの仕様違反対策も含む)での実地確認

## 6. 並列エージェント運用(実行時)

- Phase 2〜3は書き手を「モジュール境界」で分割: tree/con(+workspace/move/render) と
  x11(handlers/manage/randr) はwrite scopeが完全分離可能。bindings/config/ipcも独立。
- 各エージェントへの命令は「C関数名と1:1で対応するRust関数+docコメントにC側行数」
  を必須にし、自己判断のリライトを禁止(=そのままの移植)。
- 検証エージェントは常に別プロセス: テスト番号指定→失敗要約→原因関数の特定まで。
- 統合は lead が行う。コンフリクトはcrate境界で物理防止。

## 7. 見積り

- 人間フルタイム換算: 4〜6ヶ月(検証込み)
- この環境でエージェント並列(書き手3+検証1)を毎日回す換算: **6〜10週**
- 内訳: Phase 0-1で手触り確認、Phase 5のテスト大量失敗収束が最大の不確実性
  (経験的には「racyテストの順序依存」が半分を占める → 正規化して安定化)

## 8. このプランで「やらない」こと

- 機能追加(設定ディレクティブ・IPCメッセージ・コマンドの追加全て禁止)
- sway/wayland化、XWayland特化、GPU描画化、設定のTOML化
- Cコードの「改善」(リファクタリング・命名変更・NULLチェック追加も禁止:
  バグ含めて1:1。上流のバグ修正は上流のコミットを拾う時だけ反映)
- regexクレートへの置換(設定互換破壊のため)
- テストのi3test.pm書き直し(無変更流用が等価性の生命線)

---
付録A: 6エージェントの調査報告原文は `analysis/` 配下に保存済み
付録B: 決定保留事項は現時点でゼロ(全てPhase 0前に確定済み)
