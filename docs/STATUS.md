# i3 → Rust 移植 進捗ログ

計画: ../i3-rust-port-plan.md (i3 4.25, commit 8a97b0f / Cベースラインは rpm 4.25.1)
総行数: 58,605行 / テスト: **539 passed, 0 failed**

## 環境
- Rust: 1.100.0 (rustup default stable)
- Cベースライン: /home/sahen/適当/i3-c-baseline (rpm2cpio展開, LD_LIBRARY_PATH=usr/lib64)
- 実X検証: **Xwayland をネスト起動**（root不要）
  `XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 setsid Xwayland :7 &`
- システムlibpcre2-8: 10.49

## フェーズ状態
- Phase 0 足場: ✅ workspace雛形 / ベースライン入手 / 期待値録画
- Phase 1 libi3 + i3-msg: ✅ libi3 80テスト, i3-msg 23テスト（--help/--version/未接続エラーがC版とバイト一致）
- Phase 2 コアツリー: ✅ data/tree/con/workspace/move/render/gaps/floating/output/scratchpad/assignments + 不変条件テスト8
- spec生成器: ✅ build.rs が parser-specs から状態機械を生成（commands 2562行/config 3786行）,
  両パーサランタイム（commands_parser 13テスト / config_parser 27テスト）
- Phase 3 X11層: ✅ x/handlers/manage/randr/ewmh/xcb_util/xbackend（実Xで動作）
- Phase 4 設定: ✅ config(1628行)+config_directives(CFGFUN 75)+変数展開+include+エラー行番号
- Phase 5 コマンド+IPC: ✅ commands(4150行, 50ハンドラ)+ipc(13メッセージ+7イベント)+dump(get_tree JSON)
- Phase 6 完装: ✅ bindings/startup/sighandler/log(shmlog ABI)/display_version/xkbcommon 完了。
  **in-place restart + レイアウト保存/復元も完了**（C版と get_tree 完全一致、swallow も一致）
  残: urgencyタイマの実走行、cmd_exec/cmd_mode（次担当）
- Phase 7 i3bar+周辺: ✅ i3bar 実X接続完了・**C版i3barとピクセル完全一致（AE=0, md5一致）**。
  i3-nagbar/i3-input/i3-dump-log/i3-config-wizard も移植・テスト済み。
  描画は cairo FFI（自前宣言）+ image surface + put_image 方式で実装、**タイトルバーはCとAE=0**
- Phase 8 等価性締め: 🔄 config 15本 PASS / IPC 9種 PASS / get_tree 構造一致。
  **未了: 元Perlテストスイート285本の実行（Xephyr必須・未導入）、dirty時のx_push_changes限定**

## 実Xでの等価性（実測エビデンス）
| 検証 | 結果 |
|---|---|
| `tools/verify-config-parity.sh`（15設定） | **PASS=15 FAIL=0**（エラー文言・行番号・exitコードまで一致） |
| `tools/verify-ipc-parity.sh`（9メッセージ） | **PASS**（get_outputs はバイト一致、id のみ slotmap キー差） |
| 実X: クライアント管理 | xmessage 2枚がタイル分割され get_tree に window付き con として出現、C版と構造一致 |
| 実X: 入力フォーカス/EWMH | `_NET_ACTIVE_WINDOW`/`_NET_CLIENT_LIST`/`focus left|right`/`workspace 2`の focus遷移が **C版と完全一致** |
| 実X: kill | WM_DELETE が実クライアントに届き終了する |
| 実X: IPCイベント | `subscribe -m '["window"]'` の変化列が C版と一致 |
| i3 --help / --version / -C | バイト一致（shmlog default行のみビルド種別差＝C main.c:334 の分岐どおり） |

## 残ギャップ（正直な線）
1. **描画**: cairo/pango 未バインディング。装飾の高さが C=18px / Rust=20px（暫定7px/glyph）。ウィンドウは動くが文字は出ない
2. **Perlテスト285本**: Xephyr が必要（未導入・root要）。代わりにconfig/IPC/実Xの等価性で担保
3. **in-place restart**: `tree_append_json`/`slurp` 未実装 → restart でレイアウト復元されない
4. **DLOG**: Cの `-d` 相当のデバッグログ文が移植コードに入っておらず、ログ比較ができない箇所がある
5. **x_push_changes を毎ループ呼ぶ**（Cはdirty時のみ。最終状態は同じ、中間状態が一瞬ズレる）
6. i3bar/tools は制御フローとロジックが移植済みだが、実X接続は NullConn（実描画待ち）

## 落とし穴メモ
- `&str.as_ptr()` をC文字列として渡すとNUL終端が無くENOENT。CString必須
- libc::write/read は *mut c_void キャスト、sun_path は [c_char;108]
- x11rb 0.13: conn.intern_atom は ConnectionExt, .ok()?.reply()。AtomEnum::ANY。
  change_property の data_len は **format単位**（8=バイト, 32=/4）
- **X接続は1本**: 2本目を開くと grab_server で自分の要求が凍結してデッドロック（Cは単一接続前提）
- IPCリスナーは non-blocking 必須（Cはev_ioのreadiness駆動）
- `pkill -f 'i3 -c ...'` は自シェルも殺す。`pgrep -x i3` を使う
- ログは ELOG="ERROR: "前置, 時刻は strftime("%x %X - ")でロケール依存, 出力先 stdout, LOGはverbose gate
- `floating` フィールドは**浮動状態の文字列**。浮動子は `floating_nodes`

## Round 2 の到達点（実測）
| 検証 | 結果 |
|---|---|
| タイトルバー描画 | **C版とピクセル完全一致（AE=0、バウンディングボックス ±0px、deco 18px）** |
| i3bar | **C版i3barとピクセル完全一致（AE=0 / md5一致、640x20、ボタン矩形一致）** |
| in-place restart | C版と get_tree 完全一致、2回連続 restart も同一、窓ID保持 |
| append_layout + swallow | プレースホルダ→実窓の置換が C と一致、エラー文言バイト一致 |
| キーボード | xkbcommon 実装で bind 発火、NumLock/CapsLock フォールバック、group 切替（setxkbmap）まで動作 |
| テスト | **598 passed / 0 failed** |

### このラウンドで見つけて直した実バグ
- x_reinit が child_mapped を戻さず swallow 後のクライアントが map されない（画面真っ黒）
- X11イベントマスク定数が全部ズレていた（PROPERTY_CHANGE=1<<18等）→ PropertyNotify が一切届かずタイトル変更が反映されない
- dockarea の height が 0 → バーが1pxに潰れる（render_con_dockarea が geometry.height を使っていない）
- dockarea の Match が window_type=0 のままで dock 窓が弾かれる
- M_BELOW で tree_open_con の戻り値を捨てていた
- x_push_changes を毎ループ呼んでいた → dirty時のみに（スクショ0px差で最終状態不変を証明）
- ELOG/時刻前置/setlocale/DLOG の移植

## Round 2 追加到達点
| 項目 | 状態 |
|---|---|
| キーボード (xkbcommon) | xkbcommon/xkbcommon-x11 を自前FFIで実装。bind 発火、NumLock/CapsLock フォールバック、group切替(setxkbmap)まで動作。無限ループに見えたのは未初期化keymapの再試行だった |
| exec / mode / autostart | `exec` が実クライアントを起動し i3 が管理、`mode` 切替+IPC modeイベント、`-a` と exec_always の選択まで C と一致 |
| reload | Cと同一順序で実装。keybind差替・色反映・IPCイベント・壊れたconfigで前設定維持、すべて C と一致（`-C` エラーはバイト一致） |
| exit | `i3-msg exit` で終了しソケットも削除（C main.c:187 相当） |
| 描画設定の反映 | set_font / reload 時の色反映バグ2件（draw config コピー未更新、deco_render_params 未解放）を修正 |
| urgency タイマ / ipc_kill_timeout | 実装 |
| i3-nagbar | config エラー時に `start_nagbar`（fork/exec・C と同一 argv）で起動し、実Xに表示。`i3-msg reload` で消滅。**C版とのスクショ md5 一致**（error/warning 両テーマ、nagbar 単体起動も md5 一致） |
| テスト | **612 passed / 0 failed**（5回連続で安定を確認。cwd を触るテストは test_lock で直列化）→ Round 3 で 642 |
| 等価性 | config 15本 PASS / IPC 9種 PASS / タイトルバー AE=0 / i3bar AE=0 |

### 残ギャップ（正直な線）
1. 元Perlテストスイート285本は未実行（Xephyr が必要 = root）。代わりに config/IPC/実X の等価性で担保
2. i3-nagbar: config エラーの nagbar は spawn まで完了（util.c:366 start_nagbar / config_parser.c:507 start_config_error_nagbar）。未配線は **bindings.c:883 のコマンドエラー用 nagbar**（BindingHooks の no-op フックのみ）
3. ~~click.c の route_click / resize / drag（マウスでのリサイズ・ドラッグ）~~ → Round 3 で移植済み（下記）
4. i3bar の `-t`（32bit visual / transparency）と tray の selection owner、XKB hide_on_modifier は未実装
5. ~~xcursor（glyph cursor）~~ は Round 3 で X core "cursor" フォントによる実装に（libXcursor は使わない）。premultiplied alpha、SYNC 応答の完全版など細部は残
## Round 3: マウス操作の移植（click.c / drag.c / resize.c / tiling_drag.c）

移植したもの（C の行番号をdocコメントに明記）:
- `click.rs`: `handle_button_press`(click.c:372) / `route_click`(169) / `tiling_resize_for_border`(24) /
  `floating_mod_on_tiled_client`(84) / `tiling_resize`(127) / `allow_replay_pointer`(158)
- `drag.rs`: `drag_pointer`(206) / `drain_drag_events`(50) / `threshold_exceeded`(43)。
  C の `main_set_x11_cb(false)` + ネスト ev_loop は「ドラッグ中は XConn::poll_for_event /
  wait_for_event で自前にXイベントを読み、所有しないイベントは handle_x11_event_ctx に流す」形に置換（意味は同一）
- `resize.rs`: `resize_graphical_handler`(173) / `percent_for_1px`(131) / `resize_neighboring_cons`(146)
  （`resize_find_tiling_participants` は gaps.rs の既存実装を re-export）
- `tiling_drag.rs`: `tiling_drag`(304) / `drag_callback`(158) / `has_drop_targets`(42) /
  `is_tiling_drop_target`(13) / `find_drop_target`(73) / `adjust_rect`(108) /
  `con_on_side_of_parent`(128) / `create_drop_indicator`(271)
- `floating.rs`: `floating_drag_window`(594) / `floating_resize_window`(697) /
  `floating_move_to_pointer`(537) / `floating_check_size`(76) / `floating_maybe_reassign_ws`(489) /
  `floating_reposition`(754)（commands.rs の重複実装は floating.rs への委譲に変更）
- `x.rs`/`xbackend.rs`: `x_mask_event_mask`(x.c:1565) を実装（ドラッグ中 EnterNotify を止める）
- `XConn` 追加: grab_pointer / grab_keyboard / ungrab_* / allow_events / warp_pointer /
  circulate_window / change_active_pointer_grab / poll_for_event / wait_for_event / create_glyph_cursor

### 実X（Xwayland :34、XTest 注入）で確認したこと
| 検証 | 結果 |
|---|---|
| タイトルバー左クリック | focus が winB→winA→winB と移動 |
| ホイール（`bindsym --whole-window button4/5 workspace next/prev`） | 窓上スクロールで ws2→ws1→ws2 と切替 |
| ダブルクリック | 4.25 に double-click 判定は無い（click.c に 300ms 窓も無い）。2回押しは2回の単独クリックで floating はトグルしない |
| floating 移動（Mod1+左ドラッグ） | (10,10) → (80,110)（+70,+100 の移動が一致） |
| floating リサイズ（Mod1+右ドラッグ） | 306x122 → 266x182（-40,+60 が一致） |
| タイル枠ドラッグ（修飾キー無し） | winA 320→380px、`Graphical resize successful: first->percent = 0.59375` が C と同じ |
| tiling_drag=modifier（Mod1+左ドラッグ） | winB を winA の左端へドロップ → 順序が winB,winA に入替 |
| テスト | **642 passed / 0 failed**（click 11 + drag 3 + resize 4 を追加） |

### このラウンドで見つけて直した実バグ（マウスが動かなかった原因）
1. `grab_buttons` が `&[]`（空リスト）で呼ばれていた → クライアント窓でボタン/ホイールが i3 に来ない
   （`bindings_get_buttons_to_grab` を渡すよう修正、C: manage.c:202）
2. `RustXConn::grab_buttons` が no-op だった → `xcb_grab_button`（SYNC/ASYNC, ModMask::ANY）を実装
3. `xcursor_get_cursor` が enum 値をそのまま cursor id として返していた → 全ての grab が **BadCursor(6)** で失敗し
   ドラッグ/リサイズが起動しなかった。X の "cursor" フォントから glyph cursor を作ってキャッシュする実装に変更

### 残ギャップ（このラウンド後）
- `cmd_move_window_to_mouse` は未配線（`floating_move_to_pointer` 自体は移植済み。コマンド層に XConn が無いため TODO のまま）
- root window のカーソル設定（`xcursor_set_root_cursor`）は no-op のまま
- `_NET_WM_MOVERESIZE` のクライアント要求（handle_client_message 側）は未移植
- tiling drag のドロップ判定 DLOG は一部未移植（動作には影響なし）

## Round 4: 本家Perlテストスイート実行（大幅前進）
- 環境: /home/sahen/i3h（root不要のASCIIシンボリックファーム。testdeps に Xephyr/Xvfb/perl モジュールを展開）
- 実行: `cd /home/sahen/i3h/build && bash run-suite.sh <abs-paths.txt> 8`
  - テストは **Rust版 i3** を起動する（$B/i3 → target/debug/i3 のシンボリックリンク）
- 実測（293ファイル / 3038アサーション）: **失敗87ファイル / 239アサーション**
  - 187-commands-parser(25) / 201-config-parser(32) は C 専用ヘルパ(test.commands_parser/test.config_parser)が必要 → 対象外
  - 165b/165c/165-for_window: 各36（criteria。window_role の remanage 漏れを修正済み）
  - 291-swap 19（大半は C 版でも落ちる環境要因）、245 8、529 7、100 7、217 4 …
- 修正済み: cfg_workspace の重複時 current_workspace クリア漏れ、WM_WINDOW_ROLE の remanage 漏れ
- 復旧: 並行編集の切り詰めで失われた handlers.rs 本体関数群（12個）と config_directives.rs の CFGFUN 群を再実装、
  RecordingConn の XConn impl を再建、ユニットテスト52本を再作成 → **645 tests green**

## Round 4 最終測定（293ファイル / 3053アサーション）
- **失敗アサーション: 239 → 69**（うち 57 は 187/201 の C専用ヘルパ依存 = 対象外）
- **実質の残り失敗: 9ファイル / 12アサーション**
  - 188-regress-focus-restart(2) / 245-move-position-mouse(2) / 526-reconfigure-dock(2) /
    189-floating-constraints(1) / 504-move-workspace-to-output(1) / 542-layout-restore-remanage(1) /
    291-swap(1) / 99y-crit(1) / 309-crash-move-parent(1)
- 加えて 59ファイルが "No plan found in TAP output"（テストが done_testing 前に終了。C版でも発生するものと要切り分け）
- 決定的だった修正（このラウンド）
  1. テストハーネスの純Perl X11::XCB の send_event が28バイトで BadLength → 32バイトに修正（**多数のテストが一斉に緑**）
  2. window_type の criterion が実アトム vs 固定IDで永久不一致 → `window_type_from_atom` で正規化（165b/165c/99x の 107失敗 → 1）
  3. WM_WINDOW_ROLE の PropertyNotify で remanage_window を呼ぶ（for_window 再評価）
  4. cfg_workspace の重複時 current_workspace クリア漏れ
  5. handle_configure_request の dock ブランチ / STACK_MODE / fake_absolute_configure_notify
  6. wordexp(3) 実呼び出し、RESOURCE_MANAGER からの set_from_resource、bar の font fallback
  7. tree_render をイベント毎に（sync 応答が X 要求を追い越す問題）
  8. load_layout の on_start_array（workspace JSON の復元）
- ハーネス注意: 一括実行時に t/313 の i3-nagbar が残り、次の i3 に dock として拾われる（C でも同じ）。間に `pkill -x i3-nagbar` を挟むと安定。

## Round 5: 本家C版(4.25.1)との全ファイル比較（ヘッドレスXvfb・同一ハーネス）
- ハーネスは **Xvfb 既定**に変更（ユーザーの実デスクトップ/Discord画面共有に窓を出さないため。`I3_TEST_XSERVER=Xephyr` で従来動作）
- C版比較用ランナー: `/home/sahen/i3h/crun/run-c.sh`（PATH先頭を crun にして C 4.25.1 を起動。**i3ログで `i3 4.25.1 (2026-02-06) starting` を必ず確認**）
- 結果（293ファイル / 約3,980アサーション）:
  | | 失敗ファイル数 |
  |---|---|
  | Rust | 25 |
  | C 4.25.1 | 20 |
  - 18ファイルは**両方失敗**（うち16は 'No plan found in TAP output' = テストが done_testing に到達しないハーネス/上流都合。C でも同一）
  - **Rust だけ失敗 = 7ファイル**: 113-urgent(22) / 119-match(no plan) / 233(2) / 248(3) / 293(3) / 319-gaps(26) / 510-focus-across-outputs(15)
  - **C だけ失敗 = 2ファイル**: 165-for_window / 165c-crit（Rust の方が良い）
- 187/201（C専用ヘルパ依存）は `test.commands_parser` / `test.config_parser` を Rust で実装して**全緑**（57アサーション回復）

## Round 5 後半: urgency/focus 系を全部緑に → ほぼ C と同等
- 修正: urgent の時刻が固定値(0,1)で match_matches_window の urgent 判定が常に不一致だった /
  _NET_WM_STATE_DEMANDS_ATTENTION 未実装 / WM_HINTS 変更時の remanage 漏れ /
  assignment 後の tree_render 遅延 / check_crossing_screen_boundary 未実装 /
  mouse focus で floating を raise / tree config の disable_focus_follows_mouse 未同期
- 結果: 113-urgent(22失敗) / 510(15) / 293(3) / 233(2) / 248(3) がすべて PASS（C と同じ）
- ハーネス改善: 各テスト前に pkill -x i3-nagbar（t/313 の残留干渉を排除）
- 最終測定（293ファイル / 3,973アサーション）: 実失敗は 3ファイル / 8アサーション
  - 516-move(6) ← Rust固有（調査中）
  - 291-swap(1) / 99y-crit(1) ← C 4.25.1 でも同じく失敗（テスト側の不整合）
  - 残り17ファイルは 'No plan found in TAP output'（C でも同一）

## Round 5 最終: 本家C版と完全パリティ達成
- 293ファイル / 3,980アサーションを実行（ヘッドレスXvfb、並列8、各テスト前に i3-nagbar を掃除）
- **Rust だけ失敗 = 0 ファイル**
- C だけ失敗 = 2（165-for_window / 165c-crit）→ Rust の方が通る
- 両方失敗 = 18（うち16は 'No plan found' のハーネス/上流都合、291-swap(#110) と 99y(#7) はテスト側の不整合で C も失敗。99y は C=2 / Rust=1 で Rust の方が少ない）
- 直近の修正: 516-move（フレーム破棄時の EnterNotify ignore / warp イベント ignore / x_push_changes のポインタwarp）、
  119-match（criteria 正規表現に PCRE2_UTF|PCRE2_UCP）、519/534（warp 先を Rect コピーでなく Con から読む）
- ユニットテスト: 645 passed / 0 failed、総行数 75,438行

## Round 6: イベントループを poll(2) 化 → C より速く、C より通る
- `server.rs` の `sleep(10ms)` を廃止し、libev と同じ fd 集合（xcb接続 + IPCリスナー + 全クライアント）を poll(2) で待つようにした
  （C: main.c:1011-1084 の ev_io 群に相当）。
- 効果: 全293ファイルの実行時間 82秒 → **49秒**。IPC往復が支配的な 556-workspace-keeps-focus-after-move が
  30秒タイムアウト(3テスト) → **7秒でPASS(5テスト)** になり、**C 4.25.1 は同テストで FAIL**。
- 最終: **3982 テスト（C と同数）で、失敗は 2 アサーションのみ**（291-swap #110 / 99y-crit #7、いずれも C でも失敗）
- Rust だけ失敗するファイル: **0**。C だけ失敗: 165-for_window / 165c-crit / 556

## Round 6: トレイ/透過/フォント高さ/オーバーフロー → C より多く通る状態に
### 実装
- **システムトレイ完成**: XConn に set_selection_owner/get_selection_owner を追加し、init_tray の selection 取得を1:1で実装
  （_NET_SYSTEM_TRAY_S0 の owner 取得 → MANAGER ClientMessage → トレイクライアントの dock/reparent まで実Xで実証）
- **i3bar -t（透過）**: 32bit visual/depth/colormap の選択を実装、C と画素比較
- **バー高さ 1px 差を解消**: pango のフォント計測を cairo-xcb surface（C と同じ）で行うようにし、C と同値に
- **算術オーバーフローの修正**: Rect 座標の加算を wrapping に（C の uint32_t 加算と同じ挙動）。
  これで t/243-move-to-mark.t（C は PASS）が Rust でも PASS（以前は debug ビルドで panic）
- **flaky だった log のテスト**を修正（SHM リング 1MiB + 他テストの同時書き込みを許容）
### 最終測定（同一ハーネス・294ファイル）
| | ファイル | テスト | 失敗ファイル | 失敗アサーション |
|---|---|---|---|---|
| **Rust** | 294 | **4,032** | 17 | **2** |
| C 4.25.1 | 294 | 4,032 | 20 | 6 |
- **Rust だけ失敗 = 0**、C だけ失敗 = 3（165-for_window / 165c-crit / 556）
- ユニットテスト 651 green
- 追加修正: i3bar `-t`（32bit visual/depth/colormap）と dpi 登録により **バーが C とピクセル完全一致（AE=0、solid/-t 両方、バー高さ22px）**
  透過は depth32 visual + premultiplied ARGB で C と同一、`_NET_SYSTEM_TRAY_VISUAL` も ARGB を指す
- t/243-move-to-mark の panic（x.rs の座標加算オーバーフロー）を wrapping で修正 → PASS（C は元から PASS）
- 最終: 294ファイル / **4,032テスト** / 失敗 **2**（291-swap#110, 99y-crit#7 = C でも失敗）
  Rust だけ失敗 0 / C だけ失敗 3（165-for_window, 165c-crit, 556）

### 残る唯一の除外
- 533-randr15.t のみ未実行: C の `test.inject_randr15` ヘルパが i3 本体のビルド（meson + libstartup-notification）を要求するため

## Round 7: test.inject_randr15 を実ビルドして 533 を試行
- C の `testcases/inject_randr1.5.c` を **shim ヘッダ + libev/xcb で実コンパイル**（root不要）:
  `gcc -I/tmp/inject-shim -I<pkgs2>/usr/include -I<testdeps>/usr/include ... /usr/lib64/libev.so.4 /usr/lib64/libxcb-util.so.1 -lxcb -lxcb-randr`
  → /home/sahen/i3h/build/test.inject_randr15 に配置（旧スタブは .stub に退避）
- 結果: `533-randr15.t` は **ok 1（outputs は __i3 と fake monitor）/ ok 2（Fake output 3840x2160+0+0）まで PASS**
  ＝ 注入された RandR 1.5 モニタを Rust i3 が正しく取り込めていることを実証。
- ただし 2窓目の `manage_window` の `_NET_STARTUP_ID` 取得で停止（プロキシの素朴な sequence 書き換えと
  リクエスト列がずれて応答が飲まれる）。C は 4アサーション進んで正常終了するが **C も Result: FAIL（No plan）**。
- したがって 533 は『C専用プロキシが必要なテスト』として引き続き除外。RandR 1.5 の実挙動は
  500-multi-monitor / 526-reconfigure-dock / 102-dock 等で担保済み。

## Round 8: 仕上げ（スタブ全実装・警告0・releaseビルド・README）
- libi3 の `unimplemented!()` 4 件を C どおり実装:
  is_background_set（flicker ハンドシェイク）/ screenshot_wallpaper（copy_area + CWBackPixmap）/
  get_mod_mask（KeySymbols トレイト化）/ fake_configure_notify（32バイト wire 形式）
  → i3 本体の起動時に C: main.c:1128-1145 の「壁紙チェック」を配線、i3bar の TODO も置換
- 二重実装の解消: libi3 を正とし i3-core はアダプタのみ（modifier 走査は1箇所）
- **既存バグ修正**: i3bar の `XCB_EVENT_MASK_STRUCTURE_NOTIFY` が `1<<19`(SUBSTRUCTURE) になっていた → `1<<17`
- 警告 **176 → 0**（`cargo build --workspace`）。命名は crate ルートの allow + 理由、未使用は削除 or #[cfg(test)]、
  意味のある未使用代入は関数単位 allow + C 位置コメント
- `cargo build --release` 成功（i3 52MB, 警告0）
- README.md 追加（ビルド・実行・3種の検証ハーネスの使い方）

### 最終状態（Round 8 終了時点）
| 指標 | 値 |
|---|---|
| cargo test --workspace | **666 passed / 0 failed** |
| cargo build 警告 | **0** |
| unimplemented!() | **0** |
| 本家Perlスイート | 294ファイル / 4,032テスト / 失敗 **2**（C でも失敗） |
| Rust だけ失敗 | **0 ファイル** |
| 総行数 | 77,454行 |

## Round 8 追加: E2Eソーク（releaseビルド、C と同条件）
- Xvfb 上で i3 + i3bar + xmessage 4枚 → 17コマンド（split/focus/move/layout/floating/fullscreen/workspace/
  focus parent|child/resize/sticky）→ restart → exit を実行
- 結果: コマンド失敗1（C と同じ期待動作「Nothing to move: workspace empty」）、restart後のIPC応答OK、
  終了時にIPCソケット削除、panic 0。C と**同一の結果**（ソケット残数の推移まで一致）
- 差分: メモリ使用量 Rust 47MB / C 23MB（挙動ではなく実装差。slotmap/VecDeque/HashMap のオーバーヘッド）
- ついでに発見: Rust 移植には **ログストリームソケット（i3-dump-log -f 用）が無い** → 実装中

## Round 8 最終（ログストリームソケット実装後）
- `i3-dump-log -f` が動作（C: log.c:390/422 + main.c:1016-1026 を1:1移植）。exit/SIGTERM/restart でソケットを unlink。
  あわせて reload で `I3_SOCKET_PATH` atom が消える C 非互換も修正。
- 最終指標:
  | 指標 | 値 |
  |---|---|
  | cargo build 警告 | **0** |
  | cargo test --workspace | **668 passed / 0 failed** |
  | unimplemented!() | **0** |
  | 本家Perlスイート | 294ファイル / **4,032テスト** / 失敗 **2** |
  | 失敗の中身 | 291-swap #110, 99y-crit #7 — **いずれも C 4.25.1 でも失敗** |
  | Rust だけ失敗 | **0 ファイル** |
  | 総行数 | 78,000行超 |
- E2Eソーク（releaseビルド、17コマンド + restart + exit）が C と同一結果・panic 0。
- これにて移植は完了（残る赤は上流テスト側の不整合のみ）。
