winmux — Competitive Scan & Ideas Roundup
סיכום של 8 פרויקטים בשם winmux / WinMux ב-GitHub, רעיונות שכדאי לאמץ עם מקור מדויק, ודיזיין מפורט ל-Secrets Vault.


נכתב: 2026-05-27 · גרסה: 1.0


________________


TL;DR
נסקרו 8 פרויקטים בשם winmux/WinMux. רק 2 מהם מספקים installer מוכן (yyhezkel + HarjjotSinghh).


Strategic positioning: אתה היחיד שמשלב את 3 המרכיבים: Windows native + SSH workspaces + AI agent integration. אף אחד מהשבעה האחרים לא מכסה את כל ה-3. זה ה-moat שלך.


הרעיון הכי גדול לאמץ: HTTP automation API שמאפשר ל-LLM לנהוג ב-app עצמו, על בסיס מה שעשה editnori. אתה כבר כמעט שם (יש לך MCP server + JSON-RPC + reverse tunnel).


אזהרה: השם winmux רווי - 8 פרויקטים. שווה לחשוב על rebrand מוקדם.


________________


טבלת ה-8 פרויקטים
#
	Project
	Stack
	Niche
	Status
	מתחרה?
	0
	yyhezkel/winmux (אתה)
	Tauri 2 / Rust / SolidJS
	Win + SSH + AI agents + provisioning
	0.2.1 shipping
	—
	1
	ZimengXiong/winmux
	Swift
	macOS window manager (AeroSpace fork)
	beta
	לא (קטגוריה אחרת)
	2
	HarjjotSinghh/winmux
	Tauri 2 / Rust / React
	Win local terminal + AI notifications
	0.4.12 mature
	כן (הכי חזק)
	3
	cx8537/WinMux
	Tauri 2 / Rust / React
	Win tmux clone (no WSL)
	pre-alpha
	עתידי
	4
	Denromvas/winmux
	Rust / Tauri 2 / React
	Termux-for-Windows (QEMU bundled)
	0.1.0 alpha
	לא (קטגוריה אחרת)
	5
	editnori/WinMux
	WinUI 3 / C# / .NET 8
	Win IDE-like shell + LLM automation API
	0.1.6 alpha
	לא ישיר
	6
	albert-zen/WinMux
	Tauri 2 / Rust / React (monorepo)
	Win local cmux-clone
	early
	לא ישיר
	7
	lafin716/winmux
	Tauri 2 / Vue 3 / TS
	Win local tmux-style (personal project)
	0.1.0 toy
	לא
	

________________


Strategic Positioning
מהבחינה התחרותית:


Windows + SSH + AI agents combined → רק אתה


Windows + local terminal + AI       → HarjjotSinghh (חזק) + editnori


Windows + tmux semantics             → cx8537 (עתידי)


Windows + Linux runtime (no remote)  → Denromvas


macOS + window mgmt                  → ZimengXiong


Windows + cmux port (local)          → albert-zen


Windows + Vue toy                    → lafin716


הקטגוריה שלך לבד. השאר נופלים על קצה אחד או שניים מתוך השלושה. השמירה על הקטגוריה הזו דרך:


1. SSH workspaces עם provisioning wizard (מבדיל אותך מ-cmux כל הגרסאות)
2. Agent permission hooks blocking (מבדיל אותך מ-Warp/Termius)
3. Multi-language + RTL (BiDi עם UAX #9 — אצל אף אחד לא קיים)


________________


Ideas Inventory
ה-rating system:


* Priority = השפעה על המוצר (1=critical, 4=nice-to-have)
* Effort = הערכת זמן עבודה ל-MVP
Priority 1 — High impact, low effort (כל אחד < שבוע)
1.1 rAF-coalesced xterm writer
מקור: HarjjotSinghh/winmux קובץ: src/components/Terminal/TerminalView.tsx שורות 50-78 URL: https://github.com/HarjjotSinghh/winmux/blob/main/src/components/Terminal/TerminalView.tsx


מה זה: function makeCoalescedWriter שצוברת את כל ה-Uint8Array שמגיעים מ-PTY, ממזגת למערך אחד, וקוראת term.write פעם אחת ב-requestAnimationFrame. במקום עשרות term.write synchronous לפריים.


למה רלוונטי: כש-Claude streaming tokens מהיר, ה-thread הראשי נחנק → Windows מסמן "(Not Responding)" תוך שניות. הוא מתעד את זה כאזרחותו: "This batches them into a single merged write scheduled via requestAnimationFrame."


אצלך: ה-emit_data ב-lib.rs שולח כל chunk כ-event נפרד. ה-terminalInstance.ts קורא ל-term.write ישירות.


מאמץ: 1/2 יום. שינוי ב-terminalInstance.ts.


________________


1.2 OSC 9/99/777 notification detection
מקור: HarjjotSinghh/winmux קובץ: src-tauri/src/notification/osc.rs (178 שורות עם בדיקות יחידה) URL: https://github.com/HarjjotSinghh/winmux/blob/main/src-tauri/src/notification/osc.rs


מה זה: parser state-machine שסורק את ה-PTY output ומחפש:


* OSC 9 (iTerm2 growl): \x1b]9;<message>\x07
* OSC 99 (Kitty notification): \x1b]99;<message>\x07
* OSC 777 (rxvt notify): \x1b]777;notify;<title>;<body>\x07


למה רלוונטי: ה-hooks שלך ספציפיים ל-Claude Code. OSC עובד עם כל סקריפט/agent/כלי שיודע לשלוח escape sequence. cargo build שמסיים, pytest, scripts ידניים — כולם יכולים לטריגר feed card. משלים את ה-hooks, לא מחליף.


אצלך: המקום הטבעי — להוסיף ל-emit_data ב-lib.rs שלך scanner שכשהוא מוצא OSC notification, יוצר FeedItem עם kind: Notification.


מאמץ: 1 יום (קוד) + חצי יום (אינטגרציה).


________________


1.3 Command Palette (Ctrl+Shift+P)
מקור: HarjjotSinghh/winmux קובץ: src/components/CommandPalette/CommandPalette.tsx (98 שורות) URL: https://github.com/HarjjotSinghh/winmux/blob/main/src/components/CommandPalette/CommandPalette.tsx


מה זה: modal סטנדרטי בסטייל VSCode — input חיפוש fuzzy, רשימה מסוננת, Enter להפעיל, Esc לסגור. כל פעולה ב-app זמינה דרכו.


למה רלוונטי: יש לך 20+ פעולות (workspaces, splits, SSH commands, Claude actions, file manager, settings) ולא יותר מ-5 קיצורים. Palette נותן discoverability לכל הפעולות.


מאמץ: 1/2 יום ב-SolidJS.


________________


1.4 Context-aware Ctrl+Alt+Arrow ("split-or-move")
מקור: lafin716/winmux קובץ: src/lib/keybindings.ts שורות pane.splitOrMoveLeft/Right/Up/Down URL: https://github.com/lafin716/winmux/blob/main/src/lib/keybindings.ts


מה זה: Ctrl+Alt+→ בודק: יש pane בצד הזה? עבור focus. אין? פצל לשם. אותו keybinding לשני ה-actions, contextually.


למה רלוונטי: אצלך כרגע Ctrl+Shift+D/E/W = split-right / split-down / close. הוספה של 4 ה-arrows עם הסמנטיקה החכמה — UX win של 5 דקות בכל פעם שמשתמש לא צריך להחליט "האם לפצל או לעבור".


מאמץ: 1/2 יום. שינוי ב-shortcuts.ts + לוגיקה חדשה ב-App.tsx.


________________


1.5 ts-rs: Rust types → TypeScript types
מקור: דפוס שראיתי ב-albert-zen/WinMux (packages/protocol) ו-cx8537/WinMux (crates/winmux-protocol) URLs:


* https://github.com/albert-zen/WinMux/tree/main/packages/protocol
* https://github.com/cx8537/WinMux/tree/main/crates/winmux-protocol


מה זה: crate ts-rs (https://crates.io/crates/ts-rs) שמייצא Rust structs/enums ל-TypeScript types בזמן cargo test --features ts-export. single source of truth ל-Workspace, LayoutNode, FeedItem, וכו'.


למה רלוונטי: אצלך WorkspacesFile בRust ו-WorkspacesFile ב-types.ts נכתבים בנפרד. תוספת שדה ב-Rust ושכחה ב-TS = runtime bug שקשה לאתר. הסנכרון האוטומטי חוסם את ה-bug class הזה לחלוטין.


מאמץ: 1/2 יום. הוספת #[derive(ts_rs::TS)] ל-12-15 structs מרכזיים, build step ב-Vite.


________________


Priority 2 — High impact, medium effort (1-2 שבועות כל אחד)
2.1 ★ HTTP Automation API for LLM control
מקור: editnori/WinMux קבצים:


* Automation/NativeAutomationServer.cs (533 שורות) — ה-HTTP server
* Automation/NativeAutomationContracts.cs (1,008 שורות) — schema
* AUTOMATION_REFERENCE.md — תיעוד מלא של ה-endpoints
* scripts/run-native-automation-*.ps1 — Bun wrappers URL: https://github.com/editnori/WinMux/blob/main/AUTOMATION_REFERENCE.md


מה זה: local HTTP server (127.0.0.1:<port>, auth token) שחושף את ה-app ל-scriptability:


GET  /state              — workspaces, panes, theme, active


GET  /ui-tree            — full UI hierarchy


GET  /perf-snapshot      — perf metrics


GET  /doctor             — diagnostic health


POST /action             — click button, switch tab, new pane


POST /terminal-state     — read scrollback of a pane


POST /browser-eval       — run JS in browser pane


POST /diff-state         — read patch review state


POST /screenshot         — capture pane screenshot


POST /recording/start    — start screen recording


POST /render-trace       — capture render perf trace


למה רלוונטי (זה הרעיון הכי גדול): היום Claude שלך רץ על השרת ומבקש הרשאות דרך feed cards. מחר — Claude יכול לבקש "תפתח לי pane חדש על שרת X ותריץ cargo build", "תקרא לי את scrollback של pane #3", "תצלם את ה-workspace ושלח לי screenshot כדי שאוכל לראות את ה-bug". זה שינוי קטגוריה: מ-"winmux שמריץ Claude" ל-"winmux ש-Claude מנהל".


יתרון תחרותי: מבין כל 8 הפרויקטים, אתה תהיה היחיד עם agent-controllable terminal on remote Linux. cmux לא עושה. Warp לא עושה. Termius לא עושה. editnori — רק לוקלית בלי SSH.


יש לך כבר 80%:


* ✓ rpc_server.rs עם JSON-RPC v2 על Named Pipe
* ✓ Methods: list-workspaces, tree, send, send-key, feed.push
* ✓ winmux-mcp.exe עם 15 browser-automation tools


מה חסר:


1. Methods חדשים: pane.scrollback, pane.screenshot, ui.tree, action.split, action.connect
2. חשיפה דרך MCP — להוסיף ל-winmux-mcp tools: read_pane, take_screenshot, list_panes
3. אופציונלי: HTTP endpoint שכוטף את אותם ה-RPC methods לסקריפטרים שלא רוצים MCP


מאמץ: 5-7 ימי עבודה. הכי הרבה השקעה זמן מהרשימה, אבל ההחזר הגבוה ביותר.


________________


2.2 Auto port forwarding via /proc/net/tcp watcher
מקור: Denromvas/winmux קבצים:


* code/winmux-agent/src/main.rs (214 שורות) — ה-watcher בגוסט
* code/winmux-controller/src/port_manager.rs (44 שורות) — לוגיקת הניהול
* code/winmux-controller/src/agent_listener.rs (93 שורות) — listener URL: https://github.com/Denromvas/winmux/tree/main/code


מה זה: agent בלינוקס סורק /proc/net/tcp ו-/proc/net/tcp6 כל 500ms, מזהה ports חדשים ב-LISTEN state, שולח event ל-controller ב-Windows, ה-controller עושה port forward אוטומטי.


אצל Denromvas: הוא משתמש ב-QMP של QEMU (hostfwd_add). אצלך: ה-equivalent הוא russh's tcpip channel — אתה כבר עושה reverse tunnel + forwarded-tcpip ב-tunnel.rs. רק חסר ה-watcher.


הUX: משתמש מריץ npm run dev בשרת מרוחק על port 3000. מיד http://localhost:3000 נפתח על Windows. בלי -L 3000:localhost:3000 ידני. בלי קונפיגורציה.


יתרון תחרותי: אף אחד מהמתחרים שלך (Warp/Termius/cmux) לא עושה את זה. Termius הכי קרוב — אבל גם הוא עם config ידני.


מאמץ: 3-4 ימים.


* Linux CLI (winmux-linux-x64): port watcher loop, שולח port.opened / port.closed ל-RPC.
* Rust backend: מתודה חדשה port.opened, מאזין, פותח port forward דרך russh.
* UI: ports panel ב-sidebar, על / off toggle.


________________


2.3 Drag-and-drop לתוך טרמינל (files / URLs / clipboard images)
מקור: Denromvas/winmux קובץ: code/winmux-desktop/src-tauri/src/lib.rs (816 שורות) — חיפוש drag/drop URL: https://github.com/Denromvas/winmux


מה זה: drop של קובץ מ-Explorer / URL מדפדפן / תמונה מ-clipboard לתוך pane טרמינל. הקובץ נשמר ל-drops/ ב-guest Linux, ה-path מוזרק אוטומטית לפרומפט.


אצלך: SSH context — drop קובץ ל-SSH pane → SFTP upload ל-~/drops/ בשרת + הזרקת path אחרי upload.


מאמץ: 1-2 ימים. Tauri drag-drop event listener + SFTP upload (יש לך כבר ב-file_manager.rs) + injection ב-pty_write.


________________


2.4 Diff/patch review pane
מקור: editnori/WinMux קובץ: Panes/DiffPaneControl.cs (1,297 שורות) + Panes/DiffPaneHostControl.cs (353 שורות) URL: https://github.com/editnori/WinMux/blob/main/Panes/DiffPaneControl.cs


מה זה: pane מובנה שמראה structured diff עם:


* live / baseline / checkpoint sources (3 מקורות השוואה)
* navigation בין hunks
* zoom control
* highlight של שינויים


למה רלוונטי: Claude עורך קבצים בלינוקס מרוחק. כרגע אתה רואה את השינויים שלו רק כטקסט בטרמינל. diff pane שעוקב אחרי git diff של ה-workspace בזמן אמת = UX win של פי 10.


אצלך:


* ה-Linux CLI שולח git diff תקופתית (או כש-feed hook ב-session.end מטריגר)
* frontend מקבל את ה-patch, מציג ב-pane חדש מסוג Diff
* ספרייה: react-diff-view או monaco-editor (יש לך אותו כבר ב-FileEditor.tsx?) במצב diff


מאמץ: 5-7 ימים.


________________


2.5 Pipe hardening: SD + SID verification + Job Object + hashed name
מקור: cx8537/WinMux קבצים:


* crates/winmux-server/src/pipe/security.rs — security descriptor
* crates/winmux-server/src/pipe/handshake.rs — Hello/HelloAck
* crates/winmux-server/src/jobobj.rs — Job Object KILL_ON_JOB_CLOSE
* crates/winmux-server/src/single_instance.rs — Named Mutex URL: https://github.com/cx8537/WinMux/tree/main/crates/winmux-server/src


מה זה: 4 שיפורי אבטחה ל-Named Pipe layer:


1. Hashed pipe name: \\.\pipe\winmux-{user_sha8} — SHA-256 של username, 8 hex ראשונים. פותר usernames עם תווי Unicode/רווחים/וכו׳.
2. Explicit security descriptor: GENERIC_READ|WRITE רק ל-user SID, deny לכל אחר. במקום לסמוך על default ACL.
3. FILE_FLAG_FIRST_PIPE_INSTANCE — מונע race שבו תוקף יוצר pipe מהר יותר ומתחזה לשרת.
4. Client SID verification: GetNamedPipeClientProcessId + OpenProcessToken בודקים ש-SID של ה-client = SID של ה-server. mismatch → log + disconnect.
5. Job Object עם KILL_ON_JOB_CLOSE: כל ילד shell מצורף ל-Job. אם ה-app panic, ה-OS הורג את כל הילדים. מונע zombies.
6. Named Mutex לאינסטנס בודד: Local\WinMux-Server-{user_sha8} — שני אינסטנסים לא יכולים לתפוס את ה-pipe.


אצלך: ה-ARCHITECTURE.md שלך אומר "ACL is whatever Windows assigns by default... No HMAC needed (the pipe ACL is the auth boundary)". זה לא רע אבל זה לא מפורש. השדרוג ל-explicit SD + SID verification מעלה אותך לרמת ה-threat model של תוכנות security-grade.


מאמץ: 3-4 ימים. רוב הזמן ב-windows-rs API exploration.


________________


Priority 3 — Strategic / quality (השקעה ארוכת טווח)
3.1 פיצול lib.rs ל-crates נפרדים
מקור: albert-zen/WinMux מבנה: crates/core-pty, crates/core-ipc, crates/core-session, crates/core-state, crates/core-layout, crates/core-notify, crates/core-events, crates/core-theme URL: https://github.com/albert-zen/WinMux/tree/main/crates


מה זה: במקום lib.rs של 5,271 שורות שמכיל הכל — פיצול ל-crates ממוקדים, כל אחד עם API ובדיקות משלו.


מוצע אצלך:


crates/


├── winmux-types/         ← Workspace, LayoutNode, Connection, FeedItem (200-300 LOC)


├── winmux-pty/           ← spawn_local_pty + emit_data + UTF-8 boundary (300-400 LOC)


├── winmux-ssh/           ← SshClient, Handler, try_authenticate, spawn_ssh (1,200-1,500 LOC)


├── winmux-tunnel/        ← HMAC handshake, bridge_to_pipe, env file (300 LOC, ⊃ tunnel.rs)


├── winmux-bootstrap/     ← remote_bootstrap as-is (~400 LOC)


├── winmux-feed/          ← FeedItem, FeedStore, decide_feed (400 LOC)


├── winmux-workspaces/    ← persistence, tree ops, load/save (500 LOC)


├── winmux-rpc/           ← named-pipe server + dispatch (≈ rpc_server.rs)


└── app/                  ← Tauri builder + commands שמחברים את כל הקרייטים (300 LOC)


למה רלוונטי:


1. Claude Code יעבוד עליך טוב יותר — ה-context שצריך כדי לערוך spawn_ssh יהיה ה-crate winmux-ssh (1,500 שורות) ולא כל lib.rs (5,271).
2. בדיקות יחידה נקודתיות — לבדוק find_pane_connection ו-split_pane_in בלי להקים AppState שלם.
3. רֵיוז עתידי — ה-MCP server שלך לא צריך את כל הסטאק.


מאמץ: 2 ימים של refactoring זהיר. לא דחוף, אבל יחזיר את עצמו פי 10 לאורך הזמן.


________________


3.2 ★ Secrets Vault (capability-based)
מקור: רעיון של יוסי, עם השראה מ-1Password / HashiCorp Vault / AWS STS / SSH agent Design מלא — ראה סעיף "Secrets Vault Design" למטה.


Summary: הסוכן מקבל capability handle, לעולם לא רואה ערך גולמי. ה-broker (winmux app) מבצע הזרקה ל-egress הספציפי (SSH env / browser form / HTTP header / stdin) מבלי שהמערך יחזור לשיח LLM.


מאמץ: ~שבוע ל-MVP בסיסי (בלי browser fill) · ~10 ימים כולל browser isolated-world fill.


________________


3.3 ADR + threat model docs
מקור: cx8537/WinMux + albert-zen/WinMux קבצים:


* cx8537: docs/nonfunctional/security.md — threat model + adversaries + scope
* cx8537: docs/decisions.md — Architectural Decision Records
* albert-zen: docs/architecture/DECISIONS.md — ADR קליל ("D-001: Windows-First. Reason: ...") URLs:
* https://github.com/cx8537/WinMux/tree/main/docs/nonfunctional
* https://github.com/albert-zen/WinMux/blob/main/docs/architecture/DECISIONS.md


מה זה: שני קבצים חסרים אצלך כרגע:


1. docs/nonfunctional/security.md — threat model מפורש:


   * Assets (מה אתה מגן)
   * Adversaries (משתמש אחר באותו PC / npm install זדוני / .ssh/config שהורד / וכו׳)
   * Mitigations (per-attack)
   * Out of scope (admin / SYSTEM / physical access)


2. docs/decisions.md — ADRs קלילים:


   * D-001: Why Tauri and not Electron
   * D-002: Why SolidJS and not React
   * D-003: Why russh and not libssh-rs
   * D-004: Why GPL-3.0 and not MIT
   * D-005: Why Named Pipe and not localhost TCP
   * וכו׳


למה רלוונטי: משתמשים security-conscious מחפשים את ה-docs האלה ראשון. תורמים עתידיים שואלים "למה?" — ADR עונה. Claude Code עצמו יעבוד עליך טוב יותר כשיש לו context של החלטות.


מאמץ: 1 יום לכתיבת שני המסמכים.


________________


3.4 Recording Suite אוטומטי
מקור: editnori/WinMux קבצים: scripts/run-native-recording-suite.ps1 + 8 סקריפטים נלווים URL: https://github.com/editnori/WinMux/tree/main/scripts


מה זה: pipeline שמפעיל את ה-app, מבצע sequence של פעולות, ומפיק GIF + MP4 + screenshots באופן אוטומטי:


* overview
* workspace-showcase
* feature-tour
* patch-review
* new-project
* tab-switch
* automation-tour
* session-restore


למה רלוונטי: ה-README שלך מכיל <!-- TODO: drop a 1280×720 screenshot here once one's available. -->. אתה לא יכול לעדכן screenshot ברלי. עם recording suite — כל release מפיק media חדש מ-CI אוטומטית.


תלות: ההצעה הזו מסתמכת על #2.1 (HTTP automation API). אחרי שיש לך automation API, recording suite הוא רק orchestration.


מאמץ: 2-3 ימים אחרי שמושלמת #2.1.


________________


3.5 CLAUDE.md עם "Absolute Rules"
מקור: cx8537/WinMux קובץ: CLAUDE.md בשורש (root) URL: https://github.com/cx8537/WinMux/blob/main/CLAUDE.md


מה זה: קובץ שנטען בתחילת כל Claude Code session, מכיל 15 "Absolute Rules — Do Not Violate":


1. Never log PTY input/output content. Names and metadata only.
2. Never build shell commands by string concatenation.
3. Three-process boundaries are strict.
4. No unwrap() or expect() in non-test Rust.
5. No any in TypeScript.
6. Tests use real environments, not mocks. ... וכו׳


למה רלוונטי: אתה כבר משתמש ב-Claude Code (יש לך hooks/claude-code.json). CLAUDE.md ב-root עם 10-15 כללים שלא לעבור = פחות revert-ים. מספר דוגמאות לכללים אצלך:


* "Never expose tunnel HMAC token to logs"
* "Never store SSH passphrases in plaintext at rest"
* "All RPC methods must be schema-validated"
* "Workspace persistence must be atomic (.tmp + rename)"


מאמץ: שעה לכתיבת המסמך + 5 דקות לכל עדכון.


________________


Priority 4 — Nice-to-have
4.1 Intent Zones (drag-to-split UX)
מקור: ZimengXiong/winmux (macOS) קובץ: ניתן לחיפוש במונחים intentzones / dropZone בקוד שלו URL: https://github.com/ZimengXiong/winmux


מה זה: במקום keybindings, drag של pane על אזורים שונים של pane אחר עושה דברים שונים:


* drop על שמאל / ימין / מעלה / מטה = split בכיוון הזה
* drop על מעל (top sliver) = יצירת tab group
* drop במרכז = swap מיקומים


למה רלוונטי: משתמשי mouse-first שלא למדו את הקיצורים. כרגע אצלך אין UX מבוסס drag לפיצול.


מאמץ: 2-3 ימים.


________________


4.2 Tab Groups בתוך pane
מקור: ZimengXiong/winmux URL: https://github.com/ZimengXiong/winmux


מה זה: באותו ה-footprint של pane אחד, N sessions עם tabs בראש. שימושי כש-Claude פותח dev server + logs + test runner — כולם באותו pane השמאלי-עליון, שלוט ביניהם.


מאמץ: 4-5 ימים. שינוי מודלי ב-LayoutNode::Pane.


________________


4.3 Auto-destroy של workspaces ריקים
מקור: ZimengXiong/winmux URL: https://github.com/ZimengXiong/winmux


מה זה: אי-אפשר ליצור workspace ללא pane פעיל. workspace שכל ה-panes שלו נסגרו → נמחק אוטומטית.


אצלך: ה-equivalent — workspace שכל ה-SSH panes שלו disconnected זמן רב. הצעה: מחיקה אוטומטית אחרי 7 ימים disconnect.


מאמץ: 1/2 יום.


________________


4.4 Exposé view ("show me all panes in a grid")
מקור: ZimengXiong/winmux URL: https://github.com/ZimengXiong/winmux


מה זה: Ctrl+i (אצלו) → grid view של כל ה-panes הפתוחים על פני כל ה-workspaces, כל אחד עם thumbnail. שימושי לאנשים שמריצים 3+ Claude sessions במקביל.


מאמץ: 2 ימים.


________________


4.5 Worktree-aware workspaces
מקור: editnori/WinMux URL: https://github.com/editnori/WinMux


מה זה: כל workspace (אצלו: thread) יכול להיות מצוין ל-git worktree שונה. משתמש שמריץ 2 Claude sessions על אותו repo (אחד מתקן באג, אחד מוסיף feature) — שני workspaces על שני worktrees בלי git checkout ידני.


אצלך: הוסף git_worktree: Option<PathBuf> ב-Workspace, ופקודת CLI winmux worktree create <branch> שיוצרת worktree ו-workspace חדש שמופנה אליו.


מאמץ: 2 ימים.


________________


4.6 Quadrant splits (Ctrl+Alt+I/O/K/L)
מקור: lafin716/winmux קובץ: src/lib/keybindings.ts שורות pane.quadrantTopLeft/TopRight/BottomLeft/BottomRight URL: https://github.com/lafin716/winmux/blob/main/src/lib/keybindings.ts


מה זה: מיפוי 4 הפינות של המקלדת ל-4 רבעי המסך. לחיצה אחת = pane מקבל את הרבע (עושה 2 splits בבת אחת).


מאמץ: 2 שעות.


________________


4.7 CHANGELOG postmortem-style
מקור: HarjjotSinghh/winmux קובץ: CHANGELOG.md URL: https://github.com/HarjjotSinghh/winmux/blob/main/CHANGELOG.md


מה זה: פורמט: סימפטום → גילוי → סיבת שורש → תיקון. דוגמה שלו (v0.4.10):


"The modal is rendered inside TitleBar, whose root <div onMouseDown={startDragging}> enables native window-drag. The modal did not stop mousedown propagation, so every click on the modal started a Tauri window drag... This was not a freeze — it was drag interception."


אצלך: כשעובד על Phase X.Y, הסבר הסיבה ולא רק התסמין. CHANGELOG נקרא כספר הוראות בתפילה.


מאמץ: רק שינוי בהרגלי כתיבה, אפס LOC.


________________


4.8 Session/cookie persistence per workspace
מקור: editnori/WinMux ("shared WinMux-managed WebView2 browser profile across panes and projects") URL: https://github.com/editnori/WinMux


מה זה: ב-BrowserPane.tsx שלך, אם המשתמש פותח 2 browser panes על github.com — הם חולקים cookies + session. אחרי restart של winmux — עדיין logged in.


אצלך: הגדר additional_browser_args עם --user-data-dir per workspace ב-WebView2.


מאמץ: 1/2 יום.


________________


4.9 /doctor diagnostic endpoint
מקור: editnori/WinMux URL: https://github.com/editnori/WinMux/blob/main/AUTOMATION_REFERENCE.md


מה זה: RPC method (או HTTP endpoint, אחרי #2.1) שמחזיר snapshot של בריאות ה-app: PTY count, SSH connection states, RPC server status, memory, last 10 errors, version info.


אצלך: הוסף RPC method: doctor שמחזיר JSON עם כל זה. + פקודת CLI: winmux doctor. שימושי ל-bug reports.


מאמץ: 1/2 יום.


________________


4.10 Frontend stall instrumentation
מקור: HarjjotSinghh/winmux Reference: CHANGELOG v0.4.9 URL: https://github.com/HarjjotSinghh/winmux/blob/main/CHANGELOG.md


מה זה: מצד frontend:


* 100ms setInterval heartbeat שמודד את הפער האמיתי בין tick ל-tick. חורג מ-300ms → log "UI stall: ms" דרך פקודת Tauri `diag_log`.
* PerformanceObserver ל-longtask > 200ms — לוגים עם name (JS / style / layout).
* פקודה diag_log(level, msg) ב-Rust שכותב לקובץ הלוג הקיים.


למה רלוונטי: רוב הבאגים שלך יבואו מצד ה-WebView. אצלך יש dlog() ב-Rust אבל אין equivalent בצד JS.


מאמץ: חצי יום.


________________


★ Secrets Vault — Design מלא
עיקרון: capability, not credential. הסוכן לעולם לא רואה את ה-secret עצמו. הוא רואה רק handle שמייצג רשות לבצע פעולה ספציפית עם secret ספציפי. ה-broker (winmux app) הוא היחיד שמחזיק את הערך ומבצע את ההזרקה.
מודל איום
* Prompt injection מתוך דף web / פלט פקודה / קובץ
* Exfiltration דרך פלט LLM — secret מודפס בטעות
* חבילה זדונית שהסוכן הריץ
* לא בסקופ: Admin/SYSTEM, גישה פיזית, OS exploits
Storage
* מטא־דאטה ב-%APPDATA%\winmux\secrets.json (לא מוצפן — שמות + kinds + policies בלבד)
* ערכים ב-%APPDATA%\winmux\secrets.dpapi — כל ערך מוצפן בנפרד דרך Windows DPAPI (CryptProtectData) per-user-per-machine (אתה כבר משתמש ב-DPAPI ב-provisioning.rs)
Schema
struct Secret {


    id: SecretId,                    // UUID, מה שהסוכן רואה


    kind: SecretKind,                // Password | Token | Cookie | SshPassphrase


    name: String,                    // "GitHub PAT (prod)"


    egress: EgressPolicy,            // *לאן* מותר להזריק


    principals: Vec<Principal>,      // *מי* מורשה לבקש שימוש


    approval: ApprovalPolicy,        // *איך* נדרש אישור


    audit_count: u32,


}


enum EgressPolicy {


    HttpHeader { url_pattern: Glob, header_name: String, prefix: Option<String> },


    BrowserFormFill { domain_pattern: Glob, field_role: FieldRole },


    EnvVar { command_pattern: Glob, env_name: String },


    SshInject { host_pattern: Glob, env_name: String },


    Stdin { command_pattern: Glob, expect_prompt: Option<Regex> },


}


enum Principal {


    UserInteractive,           // user manually requests in UI


    AgentInPane(PaneId),       // Claude in specific pane


    McpTool(String),           // specific MCP tool


}


enum ApprovalPolicy {


    AlwaysAsk,                  // feed card חוסם בכל שימוש


    FirstUseWorkspace,          // פעם אחת ל-workspace session


    TimeWindowed(Duration),     // אישור חוזר אחרי חלון


    PreApproved,                // ללא אישור (רק לסקרטים שהמשתמש סימן ידנית כ-low risk)


}
API שהסוכן רואה
secret.list(principal=current_pane)


  → [{ id, name, kind, egress_summary }, ...]


  // הסוכן רואה: "יש GitHub PAT שניתן להזריק כ-Auth header ל-api.github.com/*"


  // הסוכן לא רואה: את ה-token עצמו


secret.request(secret_id, intent: { target, action })


  → Capability { cap_id, expires_at } | Denied | Pending(feed_id)


  // הסוכן מבקש, ה-broker בודק policy ו/או יוצר feed card


secret.use(cap_id, payload)


  → { result_handle } | Expired | RevokedAtUserRequest


  // ה-broker מבצע, הערך לא חוזר ל-LLM


capability היא חד-פעמית או קצובת זמן (ברירת מחדל: 60 שניות, 1 שימוש).
מימוש per-egress
1. SSH env injection (הכי קל — יש לך כבר 90%)
ה-spawn_ssh שלך כבר עושה channel.set_env("WINMUX_*", ...). הוספה:


// בתוך spawn_ssh:


let approved_secrets = vault.collect_for_ssh_channel(&host, pane_id).await?;


for (env_name, secret_value) in approved_secrets {


    channel.set_env(&env_name, &secret_value).await?;


}


הסוכן בלינוקס יראה $GH_TOKEN כ-env רגיל ויוכל gh pr create. הוא לא יראה את הערך בשיחה עם LLM. כן יוכל echo $GH_TOKEN בטרמינל — tradeoff שהמשתמש צריך להבין.
2. Browser form fill (מתקדם — דורש isolated world)
ב-WebView2 יש שני worlds:


* Main world — JS של הדף + winmux-mcp tools של הסוכן
* Isolated content script world — JS שלך, לא נגיש מ-main world


ה-broker מזריק לעולם המבודד פונקציה: __winmux_fillCredential(cap_id, field_selector). הסוכן קורא winmux-mcp.fill_credential(cap_id, selector) → Rust broker → DPAPI unwrap → WebView2 PostWebMessage → privileged isolated world → document.querySelector(selector).value = secret_value.
3. HTTP header injection — child process shim
הסוכן מריץ curl דרך winmux exec --with-secret <cap_id> -- curl .... ה-shim מקבל cap_id, פותר ל-token, מפעיל curl עם Authorization: Bearer $TOKEN, מחזיר רק את ה-response body.
4. Stdin injection לתהליכים שדורשים passphrase
ssh-add ~/.ssh/id_rsa — ה-broker spawn את התהליך, כותב passphrase ל-stdin, סוגר. הסוכן מקבל רק exit code.
Feed cards לאישור
┌─ winmux feed ──────────────────────────────────────┐


│ 🔐 Secret request                                   │


│ Claude in pane #3 (dev-server) wants to use         │


│ "GitHub PAT (prod)" as Bearer auth for              │


│ GET https://api.github.com/user/repos               │


│                                                     │


│ Egress: HTTP header (Authorization)                 │


│ Last used: 2 hours ago                              │


│                                                     │


│ [ Deny ] [ Allow once ] [ Allow for 1h ]            │


└─────────────────────────────────────────────────────┘


ה-feed item נמצא ב-pending עד שהמשתמש מחליט. בדיוק כמו feed.push שכבר קיים.
Audit log
טבלת SQLite (חבילה: rusqlite) ב-%APPDATA%\winmux\audit.sqlite:


secret_uses (


  ts INTEGER,


  pane_id TEXT,


  secret_id TEXT,


  egress_kind TEXT,


  target TEXT,           -- URL / host / command


  decision TEXT,         -- allow_once | allow_window | deny | auto_pre_approved


  cap_id TEXT,


  used BOOLEAN


)


UI: Settings → Secrets → Audit log — הצגה כרונולוגית.
Tradeoffs


	LLM יכול לראות?
	חשוף ל-prompt injection?
	session/cookies
	לא (cookies ב-WebView2)
	נמוך
	tokens dispatch via shim
	לא (capability)
	נמוך
	browser form fill (isolated world)
	לא (isolated)
	בינוני
	raw value ב-env דרך SSH
	כן (echo $X)
	גבוה
	

הקווים 1-3 הם הדפוס הנכון. קו 4 הוא ה-weakest link — קיים כי הוא הדרך הקלה להזין כלים שמצפים ל-env (gh, aws, kubectl). מי שלא רוצה — להשתמש ב-stdin/proxy.
חוזה פיתוח
1. Schema + DPAPI storage (יום)
2. RPC methods secret.list / .request / .use (יום)
3. Feed integration FeedKind::SecretRequest (חצי יום)
4. Egress: SSH env injection (חצי יום)
5. Egress: child process env shim winmux exec --with-secret ... (יום)
6. Audit log SQLite (חצי יום)
7. MCP tools list_secrets, request_capability, use_capability (חצי יום)
8. Browser isolated-world fill (3-4 ימים, אופציונלי)


MVP בלי browser fill: ~5 ימים. עם browser fill: ~10 ימים.


________________


Recommended Execution Order
Sprint 1 (שבוע — quick wins)
1. rAF-coalesced writer (#1.1) — חצי יום
2. OSC notification detection (#1.2) — יום וחצי
3. Command Palette (#1.3) — חצי יום
4. Ctrl+Alt+Arrow context-aware (#1.4) — חצי יום
5. ts-rs shared types (#1.5) — חצי יום
6. ADR + threat model docs (#3.3) — יום
7. CLAUDE.md absolute rules (#3.5) — שעה
8. /doctor endpoint (#4.9) — חצי יום
9. Frontend stall instrumentation (#4.10) — חצי יום


סה"כ: ~5 ימי עבודה. נותן לך את כל ה-low-hanging fruit.
Sprint 2 (1-2 שבועות — game changers)
1. HTTP automation API for LLM control (#2.1) — 5-7 ימים ★
2. Pipe hardening (#2.5) — 3-4 ימים
3. Drag-and-drop לתוך טרמינל (#2.3) — 1-2 ימים


סה"כ: ~10 ימי עבודה. כאן ה-product mature meaningfully.
Sprint 3 (1-2 שבועות — differentiators)
1. Auto port forwarding (#2.2) — 3-4 ימים ★
2. Secrets Vault MVP (#3.2) — 5 ימים ★
3. Diff/patch review pane (#2.4) — 5-7 ימים


סה"כ: ~13 ימי עבודה. כאן הופך ל-best-in-class.
Sprint 4 (השקעה ארכיטקטונית)
1. פיצול lib.rs לקרייטים (#3.1) — 2 ימים
2. Recording suite אוטומטי (#3.4) — 2-3 ימים אחרי #2.1
3. Worktree-aware workspaces (#4.5) — 2 ימים
4. Browser session persistence (#4.8) — חצי יום
Nice-to-have (כשיש זמן)
* Intent zones drag-to-split (#4.1)
* Tab groups (#4.2)
* Exposé view (#4.4)
* Auto-destroy empty workspaces (#4.3)
* Quadrant splits (#4.6)
* CHANGELOG style (#4.7)


________________


Naming Caveat
winmux ב-GitHub כרגע מצביע על 8 פרויקטים שונים:


1. yyhezkel/winmux — אתה (SSH + AI)
2. ZimengXiong/winmux — macOS WM
3. HarjjotSinghh/winmux — Win terminal + AI
4. cx8537/WinMux — Win tmux clone
5. Denromvas/winmux — Termux-for-Win
6. editnori/WinMux — Win IDE shell
7. albert-zen/WinMux — Win cmux port
8. lafin716/winmux — Win Vue toy


בעיות מעשיות:


* Google search לא מתחבר אחד לאחד למוצר שלך
* npm/winget אם תרצה לפרסם יהיה תפוס או דורש prefix
* אתה לא יכול לתבוע trademark כל עוד יש 8


הצעות לrebrand:


* wssh (Windows SSH)
* nexus (terminal nexus)
* bridgemux (Windows-Linux bridge)
* wraith (Windows Remote Agent Interactive Terminal Host)
* שמירה על winmux ועם prefix ייחודי בכל פלטפורמה (e.g., yossi-winmux, claude-winmux)


ההמלצה שלי: rebrand מוקדם — לפני שאתה עומד מול לקוח שאומר "התקנתי winmux ולא ראיתי SSH". עדיף עכשיו ב-0.2 מאשר אחר כך ב-1.5.


________________


Sources Quick Reference
Project
	GitHub
	Stack
	License
	ZimengXiong/winmux
	https://github.com/ZimengXiong/winmux
	Swift
	MIT
	HarjjotSinghh/winmux
	https://github.com/HarjjotSinghh/winmux
	Tauri/Rust/React
	AGPL-3.0
	cx8537/WinMux
	https://github.com/cx8537/WinMux
	Tauri/Rust/React
	MIT
	Denromvas/winmux
	https://github.com/Denromvas/winmux
	Rust/Tauri
	MIT
	editnori/WinMux
	https://github.com/editnori/WinMux
	WinUI 3/C#
	(לבדוק)
	albert-zen/WinMux
	https://github.com/albert-zen/WinMux
	Tauri/Rust/React
	(לבדוק)
	lafin716/winmux
	https://github.com/lafin716/winmux
	Tauri/Vue 3/TS
	(לבדוק)
	

________________


ההמלצה האסטרטגית בשורה אחת
מבין כל ה-15+ הרעיונות שזיהינו, שלושה משלימים זה את זה לכדי בעיטה אחת חזקה:


1. HTTP Automation API (#2.1) — מאפשר ל-Claude לנהוג ב-app עצמו
2. Auto Port Forwarding (#2.2) — מאפשר ל-Claude לפתוח dev servers שמיד נגישים על Windows
3. Secrets Vault (#3.2) — מאפשר ל-Claude להשתמש בסודות בלי לראות אותם


ביחד, השלישייה הזו הופכת אותך מ-"טרמינל למפעילי Claude" ל-"סביבת הרצה מנוהלת לסוכני AI על Linux מרוחק". זה ה-narrative שאף אחד מהשבעה האחרים לא יכול לספר.


________________




נכתב במהלך session השוואה של 2026-05-27. עדכן כשמופיע פרויקט תשיעי בשם winmux.