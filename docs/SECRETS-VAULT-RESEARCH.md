# Secrets Vault — Deep Research

> **Status:** research doc, לא spec. נכתב כדי להאיר את התכנון הקיים ב-`docs/COMPETITIVE-SCAN.md` (סעיף "★ Secrets Vault — Design מלא"), לא להחליף אותו.
> **קהל:** יוסי + Claude בסשנים עתידיים.
> **תאריך:** 2026-05-28.
> **שיטה:** WebSearch על state-of-the-art + קריאת הקוד הקיים (`provisioning.rs`, `lib.rs`). כל המקורות מרוכזים בסוף.
>
> **כלל קריאה:** Hebrew לנרטיב, English לכל שם struct / field / protocol / threat-label. ציטוטים ישירים ממקורות מקוצרים ל-≤15 מילים (copyright).

---

## 0. TL;DR — חמש מסקנות שמעצבות הכל

1. **DPAPI מבודד בין-משתמשים, לא בין-תהליכים.** `CryptProtectData` ב-`CurrentUser` scope אומר: כל תהליך שרץ תחת אותו user יכול לפענח. כלומר חבילת npm זדונית שהסוכן הריץ (postinstall) רצה כ-*אותו user* כמו ה-broker, ויכולה לקרוא את `secrets.dpapi` ולעשות `CryptUnprotectData` בעצמה — **בלי לעבור דרך ה-capability protocol בכלל**. ה-Vault מגן מצוין על *ערוץ ה-LLM* (prompt injection, exfil בצ'אט). הוא **לא** מגן מפני קוד arbitrary שהסוכן הריץ. זה בסדר — זה עדיין win גדול — אבל חייב להיאמר במפורש, והוא קובע מה מותר לסמן `PreApproved`.

2. **ל-WebView2 אין isolated worlds.** התכנון הנוכחי של `BrowserFormFill` מניח "Main world" מול "Isolated content script world" — **זה לא קיים ב-WebView2** (קיים ב-Apple WKWebView ובתוסף-דפדפן, לא ב-WebView2). סקריפט שמוזרק עם `AddScriptToExecuteOnDocumentCreated` רץ ב-main world של הדף, נגיש ל-JS של הדף. ההנחה ש-`__winmux_fillCredential` "מבודד" — שגויה. זו טעות עובדתית בתכנון, לא רק tradeoff.

3. **ה-broker pattern כבר ניצח בשוק.** Claude Code (`apiKeyHelper`), Continue.dev (Org secrets proxied, never sent to IDE), Infisical/Doppler (`run` מזריק env, כלום לא נכתב לדיסק) — כולם הגיעו לאותה מסקנה: *ה-agent מקבל use, לא value*. התזה של יוסי (capability > credential) נכונה ומאומתת ע"י התעשייה. אין צורך להמציא — צריך ליישם נכון.

4. **ה-capability לא צריך להיות JWT.** ה-broker המקומי הוא ה-verifier היחיד. אז cap_id צריך להיות **opaque handle אטום** ב-map צד-broker, single-use, נמחק ב-`.use`. זה inherently revocable ו-replay-proof, בלי חתימות, בלי key management. JWT/STS רלוונטיים רק כשיש verifier מבוזר — לא המקרה כאן.

5. **ל-MVP: שתי egress, לא חמש.** (1) **Local child-process shim** (`winmux exec --with-secret`) — מאמת את התזה הכי חזק, הסוד אף פעם לא נכנס ל-env של הסוכן. (2) **SSH env injection** — leverage הכי גבוה (90% כבר קיים ב-`lib.rs:2440`), workflow אמיתי (gh/aws/kubectl על remote). **לדחות:** BrowserFormFill (isolation שבור ב-WebView2) ו-Stdin (weakest link, niche).

---

## 1. State of the art — איך מערכות דומות מטפלות בסודות של סוכנים היום

### 1.1 כלי קוד/טרמינל עם AI

#### Claude Code (Anthropic CLI)
- **מודל הסודות:** env vars + `settings.json` תחת מפתח `env` (Record<string,string>, plain strings בלבד). API key עוקף subscription במכוון. לסודות דינמיים יש **`apiKeyHelper`** — path לסקריפט ש-Claude Code מריץ, וה-stdout משמש כ-credential.
- **מה הסוכן רואה:** את ה-env var. אבל `apiKeyHelper` הוא בדיוק ה-broker pattern: הסוד נשלף מ-1Password/Vault ב-runtime, לא hardcoded, ומסתובב מחוץ ל-config.
- **איפה זה נופל:** ה-env var הסופי גלוי לכל תהליך-ילד שה-shell מוליד. אין הפרדה בין "Claude צריך את המפתח" ל-"הסקריפט שהוא הריץ יכול לקרוא אותו". `apiKeyHelper` פותר rotation/storage, לא isolation.
- מקור: [Claude Code env vars](https://code.claude.com/docs/en/env-vars), [Manage API key env vars](https://support.claude.com/en/articles/12304248-manage-api-key-environment-variables-in-claude-code).

> **לקח ל-winmux:** `apiKeyHelper` הוא הוכחת היתכנות ל-"broker מזריק, agent לא מאחסן". winmux לוקח את זה צעד קדימה: ה-broker גם *מבצע את הפעולה* ולא רק שולף ערך.

#### Warp
- **מודל הסודות:** **Secret Redaction** — regex patterns שמזהים סודות ב-output ומצנזרים אותם *לפני* שליחה ל-LLM. כבוי כברירת מחדל, מופעל ב-Settings → Privacy.
- **מה הסוכן רואה:** פלט מצונזר. Warp מצהיר ש-"the agent does not silently read secrets from your environment".
- **איפה זה נופל:** Warp עצמם מודים — דפוסי credentials ב-shell output "too varied and context-dependent" כדי לתפוס באמינות. redaction מבוסס-regex הוא best-effort, לא boundary. וזה לא חל ב-Session Sharing.
- מקור: [Warp Secret Redaction](https://docs.warp.dev/privacy/secret-redaction), [Don't leak secrets](https://www.warp.dev/blog/dont-accidentally-leak-secrets-from-your-terminal).

> **לקח ל-winmux:** redaction הוא detection, לא prevention. winmux בחר נכון ב-capability boundary במקום regex scrubbing. אבל כדאי **גם** redaction על output של ה-shim (כשהסוד עשוי להשתקף בתשובת שרת).

#### Cursor
- **מודל הסודות:** Secrets UI מובנה (cloud/background agents) + קריאת `.env` אם מורשה.
- **מה הסוכן רואה:** הכל ב-`.env` אם לא הוחרג ב-`.cursorignore`. הקהילה (Infisical/Akeyless) ממליצה: לאחסן רק **machine identity** ב-Cursor, ולשלוף את הסוד האמיתי ב-runtime — שוב, broker pattern.
- **איפה זה נופל:** סודות "baked into disk snapshots", חוסר visibility מתי סוד נקרא/הוחלף. ובאופן מדאיג — דווח על RCE ב-Cursor דרך git hooks חבויים: הסוכן מבצע commit/checkout, hook זדוני רץ עם ה-credentials. ה-blast radius של "agent קורא .env" הוא כל הסוד.
- מקור: [Infisical: Cursor cloud agents](https://infisical.com/blog/secure-secrets-management-for-cursor-cloud-agents), [Your agent is reading your .env](https://infisical.com/blog/your-ai-coding-agent-is-reading-your-env-file), [Cursor git-hooks RCE](https://hackread.com/cursor-ai-ide-vulnerability-code-execution-git-hooks/).

> **לקח ל-winmux:** ה-git-hooks RCE הוא בדיוק תרחיש "קוד arbitrary שהסוכן הריץ = same user = יכול לפענח DPAPI". מחזק מסקנה #1.

#### OpenHands
- **מודל הסודות:** **Secret Registry** — `update_secrets()`, ה-`TerminalTool` סורק פקודות אחר מפתחות סוד ידועים, מייצא אותם כ-env vars לפני הרצה, **ו-masks את הערך ב-output**. הכל בתוך Docker container.
- **מה הסוכן רואה:** placeholder/masked ב-trace; הערך זמין כ-env לתהליך שצריך אותו.
- **איפה זה נופל:** OpenHands עצמם מתעדים שהסוכן "would read all secrets when debugging environment files". masking ב-output הוא אותו best-effort כמו Warp. ה-container מבודד filesystem, אבל סוד שהוזרק ל-env עדיין ניתן ל-exfil ע"י קוד בתוך ה-container.
- מקור: [OpenHands Secret Registry](https://docs.openhands.dev/sdk/guides/secrets), [Mitigating prompt injection](https://openhands.dev/blog/mitigating-prompt-injection-attacks-in-software-agents), [Issue #9124](https://github.com/OpenHands/OpenHands/issues/9124).

> **לקח ל-winmux:** OpenHands' "scan command → inject env → mask output" הוא בדיוק מה ש-`SshInject` עושה, אבל ל-winmux יש יתרון: ה-Docker container אצל OpenHands הוא ה-isolation; אצל winmux ה-isolation הוא ה-broker. שתי הגישות חולקות את אותו weak point: ברגע שהסוד ב-env, `echo` מדליף.

#### Aider
- **מודל הסודות:** `.env` בלבד (home / git root / cwd / `--env-file`, בסדר עדיפות), או env vars, או `--api-key provider=key`.
- **מה הסוכן רואה:** הכל. אין שכבת broker, אין isolation, אין audit.
- **איפה זה נופל:** זה ה-baseline ה"נאיבי" — credential, לא capability. כל מה ש-winmux מנסה לשפר. גם precedence של `.env` היה באג ידוע (issue #868).
- מקור: [Aider dotenv](https://aider.chat/docs/config/dotenv.html), [Aider API keys](https://aider.chat/docs/config/api-keys.html).

#### Continue.dev
- **מודל הסודות:** Mission Control עם **User Secrets** ו-**Org Secrets**. שימוש דרך `${{ secrets.NAME }}`.
- **מה הסוכן רואה:** למפתח: ה-IDE extension *לא* מקבל את הערך כש-Org secret. "LLM requests are proxied through api.continue.dev and secrets are never sent to the IDE extensions". זה ה-broker pattern במלואו — הסוד יושב בצד שרת, הבקשות עוברות proxy.
- **איפה זה נופל:** דורש control-plane מבוזר (api.continue.dev). אופליין/local-first זה לא מתאים. אבל הרעיון — "proxy את הבקשה, אל תיתן את הסוד" — הוא בדיוק ה-HTTP-header shim של winmux, מקומית.
- מקור: [Continue.dev Secret Types](https://docs.continue.dev/mission-control/secrets/secret-types).

> **לקח ל-winmux:** Continue.dev מוכיח שמשתמשים מקבלים "proxy את הבקשה" כ-UX. winmux עושה את אותו דבר בלי שרת — ה-broker המקומי הוא ה-proxy.

#### GitHub Copilot CLI
- **מודל הסודות:** שלוש שיטות: OAuth device flow (ברירת מחדל אינטראקטיבית), env vars (`COPILOT_GITHUB_TOKEN` > `GH_TOKEN` > `GITHUB_TOKEN`), ו-`gh` CLI fallback. דורש **fine-grained PAT** עם הרשאת "Copilot Requests"; classic tokens (`ghp_`) מתעלמים בשקט.
- **מה הסוכן רואה:** ה-token דרך env או OAuth-stored. אין הפרדה בין "Copilot צריך לדבר עם ה-API" ל-"ה-token זמין לכל פקודה".
- **איפה זה נופל:** env-var-based, אז same-process-tree exposure. היתרון היחיד: הם **כופים fine-grained PAT** — scope-down ברמת ה-credential.
- מקור: [Authenticating Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli).

> **טבלת סיכום — כלי AI:**
>
> | כלי | מודל | agent רואה value? | boundary |
> |---|---|---|---|
> | Aider | `.env` | כן | אין |
> | Copilot CLI | env / OAuth | כן (env) | scope ב-PAT |
> | Warp | redaction | מצונזר (best-effort) | regex |
> | OpenHands | Secret Registry | masked (best-effort) | Docker + masking |
> | Claude Code | env + `apiKeyHelper` | כן (env) | broker ב-fetch |
> | Continue.dev | proxy (Org) | **לא** (Org) | server-side proxy |
> | **winmux (מוצע)** | **capability** | **לא** | **local broker** |
>
> winmux ו-Continue.dev הם היחידים עם boundary אמיתי. winmux היחיד שעושה זאת local-first.

### 1.2 אקוסיסטם של secrets managers

#### 1Password CLI
- **גישה בלי master password בכל פעם:** שלוש דרכים. (a) **Service account token** ב-env var — לאוטומציה headless. (b) **Biometric unlock** — Windows Hello / fingerprint במקום הסיסמה. (c) **App integration** — `op` מתחבר לאפליקציית 1Password ש-mngs את ה-unlock. בנוסף **SSH agent** מובנה + Shell Plugins שעוטפים CLIs שלמים ב-biometric.
- **דפוס ל-winmux:** ה-biometric-per-use הוא בדיוק מה שצריך ל-egress רגיש. ה-Shell Plugins ("עטוף CLI, בקש מגע") הם תאומים ל-`winmux exec --with-secret`.
- מקור: [1Password biometric unlock](https://developer.1password.com/docs/cli/use-biometric-unlock/), [Shell Plugins](https://1password.com/blog/shell-plugins).

#### HashiCorp Vault
- **broker pattern קלאסי:** AppRole (RoleID + SecretID → token), כל סוד דינמי מקבל **lease** עם TTL, renewable עד `token_max_ttl`, ו-**revocable** (מיידית, idlocalinvalidate). "All dynamic secrets in Vault are required to have a lease".
- **מה זה ה-"capability" המקביל:** ה-**lease_id** + ה-token. ה-token הוא ה-handle; ה-lease הוא ה-TTL/revocation binding.
- **לקח ל-winmux:** ה-lease model = ה-`Capability { cap_id, expires_at }` של יוסי, פלוס revocation. winmux צריך לאמץ את "כל cap הוא lease שניתן לבטל מה-UI".
- מקור: [Vault Lease](https://developer.hashicorp.com/vault/docs/concepts/lease), [Vault AppRole](https://developer.hashicorp.com/vault/docs/auth/approle).

#### AWS STS
- **`AssumeRole` + session policy:** מחזיר temporary credentials (access key + secret + **session token**). ה-session policy עושה **scope-down**: ההרשאות הן ה-*intersection* של role policy ו-session policy — אי אפשר להעלות הרשאות, רק לצמצם.
- **binding ל-session:** ה-session token חייב להישלח עם כל קריאה; AWS מאמת אותו. short-lived מובנה.
- **לקח ל-winmux:** ה-"intersection, never escalate" הוא העיקרון הנכון ל-`secret.request(intent)`: ה-intent יכול רק לצמצם את ה-`EgressPolicy`, לעולם לא להרחיב. cap צריך לקודד את ה-intersection בין מה שהסוד מתיר למה שהבקשה ביקשה.
- מקור: [STS AssumeRole](https://docs.aws.amazon.com/STS/latest/APIReference/API_AssumeRole.html).

#### Doppler / Infisical
- **runtime injection:** `infisical run -- <cmd>` מזריק סודות כ-env vars לתהליך. "Nothing is written to disk... credentials fetched fresh on every agent boot". **Machine Identity** (non-human principal) מאומת דרך Universal Auth / OIDC / cloud IAM, scoped לסודות שהסוכן צריך בלבד. Doppler מזריק env בלבד (אין SDK fetch-by-name); Infisical גם fetch פר-שם.
- **לקח ל-winmux:** "machine identity scoped to agent" = ה-`Principal` של winmux. "inject env, nothing to disk, fresh per boot" = בדיוק מה ש-`SshInject`/shim צריכים לעשות. וההבדל Doppler-vs-Infisical (env-only מול fetch-by-name) ממפה ל-winmux: env injection (כמו Doppler) מול capability-use (כמו Infisical SDK).
- מקור: [Infisical CLI secrets](https://infisical.com/docs/cli/commands/secrets), [Cursor cloud agents](https://infisical.com/blog/secure-secrets-management-for-cursor-cloud-agents).

#### age (FiloSottile)
- **keypair pattern:** X25519 recipients (public) / identities (private). file key אקראי (16 bytes) נעטף פעם לכל recipient. אפשר להצפין לכמה recipients במקביל.
- **לקח ל-winmux:** age רלוונטי **לא** ל-runtime use אלא ל-**backup/export של ה-vault** או **sync בין מכונות**: הצפן את ה-secrets store ל-recipient שהוא TPM-backed key או YubiKey. DPAPI לא ניתן להעברה בין מכונות (per-machine); age כן. זו דרך נקייה ל-"export vault" בלי PowerShell PGP.
- מקור: [age authentication](https://words.filippo.io/age-authentication/), [FiloSottile/age](https://github.com/FiloSottile/age).

---

## 2. אבני בניין טכניות — מה זמין בפועל ב-Windows

### 2.1 DPAPI (`CryptProtectData` / `CryptUnprotectData`)
- **כבר בשימוש:** `provisioning.rs:255` עוטף סיסמת-init דרך **shell-out ל-PowerShell** (`ProtectedData::Protect(..., 'CurrentUser')`), הסוד מועבר ב-env var `WINMUX_SECRET` ל-`powershell.exe`.
- **חוזק:** per-user-per-machine, אפס key management, ה-master key נגזר מ-logon credentials. העברת ה-JSON למשתמש אחר = ג'יבריש.
- **חולשה קריטית:** `CurrentUser` נותן בידוד **בין-משתמשים בלבד**. "any application running on your credentials can access the protected data". אין intra-user process isolation. תהליך זדוני באותו user → `CryptUnprotectData` על `secrets.dpapi` → plaintext. בנוסף DPAPI נוצל היסטורית לחילוץ סודות ארגוני דרך ה-domain backup key.
- **חולשת המימוש הנוכחי:** ה-shell-out ל-PowerShell חושף את הסוד ב-env block של תהליך-ילד (קריא ל-same-user דרך `NtQueryInformationProcess`/WMI), ומשאיר את הסוד plaintext רגעית ב-process אחר. ל-vault בתדירות גבוהה — **לעבור ל-`windows-rs` native in-process** (כמו שהעדכון עבר מ-PowerShell ל-`ureq` ב-v0.2.3 לפי CLAUDE.md). אין env var, אין תהליך-ילד.
- מקור: [DpapiDataProtector.Scope](https://learn.microsoft.com/en-us/dotnet/api/system.security.cryptography.dpapidataprotector.scope), [CryptProtectData](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata), [Sygnia: DPAPI downfall](https://www.sygnia.co/blog/the-downfall-of-dpapis-top-secret-weapon/).

### 2.2 Credential Manager מול DPAPI
- **Credential Manager משתמש ב-DPAPI מתחת.** `CredRead`/`CredEnumerate` נגישים לכל תהליך ב-user context. אז Credential Manager **לא** פותר את בעיית ה-intra-user isolation — אותה חולשה בדיוק, פלוס API נוח יותר לתוקף (`CredEnumerate` מונה הכל).
- **מתי בכל זאת:** Credential Manager טוב כשרוצים אינטגרציה עם מנגנוני OS (RDP, git credential helper). ל-vault פרטי של winmux — DPAPI ישיר על קובץ נשלט עדיף, כי אין enumeration surface ואין הופעה ב-`vaultcmd`/UI של Windows.
- **האם הסוכן יכול להידחות גישה?** לא ברמת ה-OS — שניהם same-user. הדחייה חייבת לבוא מה-broker (logical), לא מה-storage.
- מקור: [Dumping creds w/ SeTrustedCredmanAccess](https://www.tiraniddo.dev/2021/05/dumping-stored-credentials-with.html).

### 2.3 TPM-backed keys / Windows Hello / Virtual Smart Card
- **למה זה חשוב:** זו הדרך היחידה לשבור את ה-intra-user weakness. מפתח ב-TPM **לא ניתן לחילוץ** — תהליך זדוני יכול לבקש *שימוש* בו אבל לא להעתיק אותו, וב-Windows Hello השימוש מותנה ב-user gesture (PIN/biometric).
- **headless בלי biometric:** TPM Virtual Smart Card עובד עם **PIN** (לא חובה biometric) — מתאים ל-laptop בלי מצלמת IR/קורא טביעה. נוצר עם `Tpmvscmgr` או `Windows.Device.SmartCards`. Microsoft ממליצים לעבור ל-Windows Hello for Business / FIDO2, אבל ה-VSC עדיין נתמך.
- **דפוס ל-winmux:** עטוף את ה-DPAPI master של ה-vault במפתח TPM-sealed. אז גם same-user malware שקורא `secrets.dpapi` לא יכול לפענח בלי gesture/PIN דרך ה-TPM. זו ה-upgrade path מ-MVP ל-hardened.
- מקור: [Virtual Smart Card overview](https://learn.microsoft.com/en-us/windows/security/identity-protection/virtual-smart-cards/virtual-smart-card-overview), [Windows Hello apps](https://learn.microsoft.com/en-us/windows/apps/develop/security/windows-hello).

### 2.4 WebView2 isolated worlds — **התיקון הגדול**
- **העובדה:** ל-WebView2 **אין** isolated worlds. הוא יכול להריץ קוד רק ב-page context ("main" world), בניגוד ל-Apple platforms. `AddScriptToExecuteOnDocumentCreated` רץ באותו context כמו הדף — אם הדף משנה `console.log`, השינוי משתקף גם בסקריפט המוזרק (הוכח ב-WebView2Feedback #2510). Microsoft עצמם מזהירים: "Be careful with `AddScriptToExecuteOnDocumentCreated`... any HTML document may have access to the native application's resources".
- **מה זה שובר בתכנון:** ה-`__winmux_fillCredential(cap_id, selector)` המוזרק יושב ב-main world. דף זדוני יכול: (a) לעטוף/לדרוס את `document.querySelector(...).value` setter ולקרוא את הסוד ברגע ההזרקה, (b) לקרוא `input.value` אחרי המילוי, (c) לדרוס את הפונקציה הגלובלית עצמה. ההנחה "isolated content script world" שגויה.
- **מה כן אפשר ב-WebView2:** למלא דרך **CDP / DevTools Protocol מצד ה-host** (`Runtime.evaluate`/`Input.insertText`) ולא דרך global מוזרק; להריץ ב-WebView2 environment ייעודי לכל credential-fill; למלא ואז *מיד* לשגר את הטופס (submit) כדי לצמצם חלון הקריאה; אף פעם לא לחשוף callable global. אבל **residual risk נשאר**: דף שמאזין ל-`input` events או עוטף setters יכול לראות שהמילוי קרה ואת הערך. ל-credential לתוך דף לא-מהימן — זה **High**, לא "בינוני" כמו בתכנון.
- מקור: [WebView2 security](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/security), [WebView2Feedback #2510](https://github.com/MicrosoftEdge/WebView2Feedback/issues/2510).

### 2.5 SQLite-with-encryption (SQLCipher) — לאודיט
- **השאלה:** צריך SQLCipher אם DPAPI כבר ה-boundary? **לא, ולא מהסיבה שנדמה.** הבעיה באודיט היא **integrity/tamper-evidence**, לא confidentiality. SQLCipher נותן הצפנה — אבל same-user מחזיק את המפתח, אז הוא יכול גם לקרוא וגם לכתוב מחדש. הצפנה ≠ שלמות.
- **מה כן צריך:** **SHA-256 hash chain** — כל שורה כוללת hash של עצמה + ה-hash הקודם. שינוי שורה שובר את השרשרת לכל מה שאחריה. זה "the same approach used in git commits, certificate transparency". אבל "a sufficiently powerful attacker with full database access can modify past events, recompute hashes" — אז ה-**chain head** חייב להיות מעוגן מחוץ להישג ה-same-user: TPM-sealed, או append-only חיצוני, או נשלח ל-feed שכבר persisted.
- **המלצה:** rusqlite רגיל (לא SQLCipher) + hash-chain column + עיגון ה-head. הצפנה רק אם ה-`target` (URL/host) עצמו רגיש.
- מקור: [Tamper-evident audit log w/ hash chains](https://dev.to/veritaschain/building-a-tamper-evident-audit-log-with-sha-256-hash-chains-zero-dependencies-h0b), [Crosby & Wallach, USENIX Security '09](https://static.usenix.org/event/sec09/tech/full_papers/crosby.pdf).

### 2.6 Hardware tokens (YubiKey OpenPGP / gpg-agent)
- **"proof, not possession":** המפתח הפרטי נשאר על ה-OpenPGP card; רק link מיובא ל-GPG. "your keys cannot be compromised in the case of an infection". gpg4win תומך ב-SSH auth: חיבור ראשון = PIN + מגע, אחר כך רק מגע.
- **מקום ב-winmux:** ה-tier הגבוה — egress שדורש הוכחה קריפטוגרפית של נוכחות (לא רק "התהליך ביקש"). gotcha: gpg4win פותח smart cards ב-exclusive mode → מתנגש עם Pageant-replacements. אז אינטגרציה צריכה זהירות.
- מקור: [YubiKey SSH auth on Windows](https://developers.yubico.com/PGP/SSH_authentication/Windows.html).

### 2.7 Memory-only secrets (`zeroize` / `secrecy`)
- **`zeroize`:** `ptr::write_volatile` + memory fences, מבטיח שאיפוס לא יוסר ע"י optimizer. אבל מודה: "potential for microarchitectural attacks (Spectre/Meltdown) to leak" — לא מגן מ-covert channels חומרתיים.
- **`secrecy`:** עוטף ב-`SecretBox`/`SecretString`, מאפס ב-drop, **redaction ב-Debug** (מונע logging בטעות — קריטי לכלל #1 של winmux). אבל no_std-friendly: **לא** עושה `mlock`/`mprotect` — הסוד יכול להגיע ל-swap/hibernate file.
- **כמה הזיכרון דליף בפועל:** סוד ב-process memory ניתן ל-MiniDump (same-user/admin). page file + `hiberfil.sys` יכולים לתפוס אותו לדיסק. `zeroize` מקצר את החלון אבל לא סוגר hibernate. ל-winmux: עטוף כל secret value ב-`secrecy::SecretString`, אפס מיד אחרי ה-egress, ושקול `VirtualLock` (windows-rs) על ה-buffer לסודות הכי רגישים.
- מקור: [zeroize](https://docs.rs/zeroize/latest/zeroize/), [secrecy](https://docs.rs/secrecy/latest/secrecy/).

---

## 3. מודל איום — STRIDE פורמלי

הנחת בסיס: התוקף הוא **same-user code** (חבילה זדונית שהסוכן הריץ, prompt injection שגרם להרצה) או **ה-LLM channel** (exfil בצ'אט). מחוץ לסקופ: Admin/SYSTEM, גישה פיזית, OS exploits, evil-maid — בדיוק כמו בתכנון של יוסי.

### 3.1 Spoofing
- **T-S1:** binary זדוני בשם `winmux-mcp.exe` מתחבר ל-named pipe / local socket של ה-broker וקורא `secret.request`. ה-`Principal::McpTool(String)` הוא **string חופשי** — trivially spoofable. אין authentication של ה-peer.
- **T-S2:** תהליך מתחזה ל-pane אחר ע"י העברת `pane_id` שקרי ב-`secret.request(principal=current_pane)` — ה-API מקבל את ה-claim של הקורא.
- **כיסוי בתכנון:** ❌ לא מכוסה. ה-Principal enum מתאר *מי מורשה* אבל לא *איך מאמתים את הזהות*.
- **mitigation נדרש:** named pipe עם ACL מוגבל ל-SID של ה-user + בדיקת image path/signature של ה-peer process (`GetNamedPipeClientProcessId` → `QueryFullProcessImageName`). את `pane_id` ה-broker חייב **לגזור מהחיבור המאומת**, לא מה-claim של הקורא.
- **residual:** same-user malware עדיין יכול להתחזות ל-MCP אם הוא מזייף את ה-image path (אפשרי ל-same-user). לכן spoofing מלא נמנע רק עם code-signing enforcement — לא ב-MVP. **residual: בינוני-גבוה.**

### 3.2 Tampering
- **T-T1:** `secrets.json` (metadata, **לא מוצפן** לפי התכנון) נערך ע"י same-user process — מוסיף `EgressPolicy` מתירני, או הופך `ApprovalPolicy` ל-`PreApproved`, או משנה `url_pattern` ל-endpoint של התוקף (ואז ה-broker "בתום לב" ישלח את ה-Bearer header ליעד הזדוני).
- **T-T2:** עריכת `workspaces.json` (כלל #7 של winmux כבר דורש atomic writes — רלוונטי).
- **כיסוי בתכנון:** ⚠ חלקי. ה-values מוצפנים (DPAPI), אבל ה-**policy** גלוי וניתן לעריכה. זו ה-attack surface האמיתית: לא צריך לפענח את הסוד אם אפשר לשנות *לאן* הוא נשלח.
- **mitigation נדרש:** HMAC/sign על `secrets.json` עם מפתח ב-DPAPI — אבל same-user יכול לחתום מחדש. עיגון אמיתי דורש TPM. חלופה קלה ל-MVP: לאחסן את ה-policy **בתוך אותו DPAPI blob** כמו הערך (אז tampering הורס את שניהם), ולוודא שה-`url_pattern` הוא allowlist קשיח שה-user אישר, לא glob פתוח.
- **residual: בינוני** (גבוה אם נשאר metadata גלוי).

### 3.3 Repudiation
- **T-R1:** `audit.sqlite` הוא same-user-writable. תוקף מוחק שורות, או הופך `decision` מ-`deny` ל-`allow_once`, או מוחק `used=true`. המשתמש יכול להכחיש שאישר; או תוקף מסתיר שימוש.
- **כיסוי בתכנון:** ❌ אין tamper-evidence על האודיט.
- **mitigation נדרש:** hash-chain (3.5) + עיגון ה-head מחוץ ל-same-user (TPM-sealed counter, או mirror ל-feed שכבר persisted, או append-only file עם `FILE_APPEND_DATA` בלבד ב-ACL).
- **residual: בינוני** — ניתן לזהות tampering, לא למנוע.

### 3.4 Information disclosure
- **T-I1 (LLM channel):** הסוד מודפס בטעות בתשובת ה-shim. דוגמה ממשית: `winmux exec --with-secret -- curl https://httpbin.org/headers` — השרת משקף את ה-`Authorization` header חזרה ב-body, וה-body חוזר ל-LLM. **ה-broker חייב לסרוק את ה-output של ה-shim ולצנזר את ערך הסוד לפני החזרה.**
- **T-I2 (`echo $X`):** SSH env injection — `echo $GH_TOKEN` בטרמינל מדליף raw. מאושר בתכנון כ-tradeoff.
- **T-I3 (memory dump):** סוד ב-process memory של ה-broker → MiniDump (same-user) → plaintext. page file / `hiberfil.sys` יכולים לשמר. mitigation: `secrecy::SecretString` + zeroize + (אופציונלי) `VirtualLock`.
- **T-I4 (PowerShell shell-out):** המימוש הנוכחי (`provisioning.rs:255`) מעביר את הסוד ב-env var של `powershell.exe` — קריא ל-same-user רגעית. mitigation: native in-process DPAPI.
- **T-I5 (cross-pane echo):** סוד שהוזרק ל-pane אחד מודפס ל-shared tmux/scrollback שנגיש מ-pane אחר.
- **כיסוי בתכנון:** ✅ ל-I1 חלקית (capability), ⚠ ל-I2 (מאושר), ❌ ל-I3/I4/I5.
- **residual: גבוה ל-SSH env (by design), נמוך ל-shim אם output scrubbing מיושם.**

### 3.5 Denial of service
- **T-D1:** capability-request flood → feed cards מצטברים (ב-pending לפי התכנון). ה-UI מוצף, המשתמש מאבד אמון/מאשר בטעות (ראה MFA fatigue, סעיף 5).
- **T-D2:** ה-cap_id map גדל ללא הגבלה אם caps לא נמחקים/פגים.
- **mitigation נדרש:** rate-limit per principal, תקרה על pending feed items (auto-deny אחרי N), TTL מחיקה אגרסיבית של caps שלא נוצלו.
- **residual: נמוך** אם rate-limiting מיושם.

### 3.6 Elevation of privilege
- **T-E1:** pane בעל trust נמוך (`AgentInPane(pane_2)`) מבקש cap לסוד ש-scoped ל-`pane_1`. אם ה-broker בודק principal רק ב-`.request` אבל לא ב-`.use`, או אם הוא סומך על ה-`pane_id` שהקורא העביר — escalation.
- **T-E2:** cap שהונפק ל-intent מצומצם משמש ל-intent רחב יותר (אם ה-cap לא קושר חזק ל-intent).
- **mitigation נדרש:** bind כל cap ל-`(authenticated_principal, secret_id, intent_hash, audit_id)`; אמת principal **גם** ב-`.use`; ה-`pane_id` נגזר מהחיבור (ראה 3.1), לא מה-payload.
- **residual: נמוך** אם binding + re-check מיושמים.

> **סיכום STRIDE:** ה-boundary של ה-vault חזק מול **LLM exfil** (התזה המקורית). הוא חלש מול **same-user code execution** בגלל DPAPI intra-user weakness. שתי החולשות הכי חשובות שהתכנון לא מכסה: (a) **peer authentication** של ה-MCP/pane (Spoofing+EoP), ו-(b) **tamper-evidence** של policy+audit (Tampering+Repudiation). שתיהן ניתנות לתיקון בלי TPM ברמת "detect", ועם TPM ברמת "prevent".

### 3.7 שני תרחישי תקיפה מלאים (worked attacks)

שני התרחישים האלה הם הקרש שמכריע את הארכיטקטורה. כדאי לבנות PoC לשניהם לפני implementation.

**Attack A — npm postinstall קורא את ה-vault ישירות (עוקף את ה-broker לגמרי):**
1. הסוכן (אחרי prompt injection, או סתם בתום-לב) מריץ `npm install some-pkg`.
2. ל-`some-pkg` יש `postinstall` script זדוני. הוא רץ כ-**אותו Windows user** כמו `winmux.exe`.
3. ה-script קורא את `%APPDATA%\winmux\secrets.dpapi` (קובץ נגיש ל-same-user).
4. ה-script קורא ל-`CryptUnprotectData` על כל blob — **מצליח**, כי DPAPI `CurrentUser` מפענח לכל תהליך של אותו user.
5. כל הסודות בפליינטקסט. ה-capability protocol, ה-feed cards, ה-audit — **כולם לא נגעו בכלל**. אין שורת audit, כי לא עברו דרך ה-broker.
- **מה עוצר את זה היום:** כלום.
- **מה מצמצם:** TPM-sealing של ה-master (צעד 4 נכשל בלי gesture/PIN). זה ה-upgrade שהופך את ה-vault מ-"מגן על ה-LLM channel" ל-"מגן גם מ-same-user code". **זו הסיבה ש-TPM הוא Increment 1, לא nice-to-have.**
- **מה לא מצמצם:** capability protocol מתוחכם יותר — התוקף לא משתמש בו.

**Attack B — דף זדוני קורא את הערך שמולא (BrowserFormFill):**
1. ה-broker מזריק `__winmux_fillCredential` ל-WebView2 דרך `AddScriptToExecuteOnDocumentCreated` → רץ ב-**main world** (סעיף 2.4).
2. הסוכן קורא `fill_credential(cap_id, "#password")`.
3. *לפני* המילוי, ה-JS של הדף כבר הריץ `Object.defineProperty(input, 'value', { set(v){ exfil(v); ... } })` או הוסיף `input` event listener.
4. ה-broker עושה `document.querySelector("#password").value = secret` → ה-setter הזדוני יורה → הסוד נשלח ל-`exfil()`.
5. גם בלי setter override: `input.addEventListener('input', e => exfil(e.target.value))` תופס את אותו דבר.
- **מה עוצר את זה היום:** ההנחה ש-WebView2 מבודד — **שגויה**. כלום לא עוצר.
- **מה מצמצם חלקית:** fill דרך CDP `Input.insertText` host-side (לא דרך global מוזרק) — אבל events עדיין יורים, אז דף שמאזין עדיין רואה. fill-then-immediately-submit מצמצם את החלון אבל לא סוגר.
- **מסקנה:** BrowserFormFill לתוך דף לא-מהימן הוא High risk מובנה ב-WebView2. לדחות מ-MVP.

---

## 4. Capability protocol — סקירה + ביקורת

התכנון: `secret.request → Capability { cap_id, expires_at }` ואז `secret.use(cap_id, payload)`, ברירת מחדל 60s / שימוש יחיד.

### 4.1 השוואה
| מנגנון | shape | TTL/revocation | replay protection | מתאים ל-winmux? |
|---|---|---|---|---|
| **JWT** | self-contained signed claims | exp claim; revocation קשה (צריך blocklist) | jti + exp | ❌ overkill — אין verifier מבוזר |
| **AWS STS token** | opaque + session policy | short-lived; scope-down intersection | server validates each call | רעיון ה-intersection ✅ |
| **Vault lease** | lease_id + token | TTL renewable + revoke מיידי | server-side state | מודל ה-lease ✅✅ |
| **Capsicum (FreeBSD)** | OS capability fd, unforgeable | מוגבל ל-lifetime של ה-fd | OS-enforced, אי אפשר לזייף | השראה: cap = unforgeable handle ✅ |
| **WebAuthn assertion** | signed challenge, UP/UV flags, counter | one-time challenge | counter > stored, fresh challenge | ל-egress רגיש בלבד ✅ |

### 4.2 הצורה הנכונה ל-winmux
ה-broker המקומי הוא ה-verifier **היחיד**. אז:

- **opaque handle, לא token חתום.** `cap_id` = 128-bit random (`Uuid::new_v4` או `getrandom`), מפתח ב-`HashMap<CapId, PendingCap>` צד-broker. אין חתימה, אין key management. inherently revocable (מחק מה-map), inherently replay-proof (מחק ב-`.use`).
- **single-use as default,** N-use רק ל-egress מפורש (למשל "1h window" ל-read-only GET). ה-`PendingCap` נמחק ב-consume.
- **שדות מומלצים:**
  ```rust
  struct PendingCap {
      cap_id: CapId,              // 128-bit random
      secret_id: SecretId,
      principal: AuthnPrincipal,  // נגזר מהחיבור, לא מה-claim
      intent: Intent,             // { egress_kind, target, action } — frozen at request
      intent_hash: [u8; 32],      // bind ל-.use payload
      issued_at: Instant,
      expires_at: Instant,        // default issued_at + 60s
      uses_left: u32,             // default 1
      audit_id: AuditId,          // קישור לרשומת האודיט
  }
  ```
- **replay protection:** single-use + מחיקה אטומית ב-`.use` (תחת mutex). גם אם cap_id דלף — חד-פעמי וקצר-מועד.
- **audit binding:** ה-`audit_id` נוצר ב-`.request` (decision=pending), מתעדכן ב-`.use` (used=true). הקישור cap↔audit הוא bidirectional — אי אפשר use בלי audit row.
- **TTL default:** 60s סביר ל-interactive; שקול 30s ל-single-use רגיש. ל-window-based (read-only) — 1h עם uses מרובים, אבל עם re-check של ה-principal בכל use.
- **intent freezing (לקח מ-STS):** ה-`intent` ננעל ב-`.request`. ה-`payload` ב-`.use` חייב להתאים ל-`intent_hash` — אי אפשר לבקש cap ל-`GET /user` ולהשתמש בו ל-`POST /repos/.../delete`.
- **user-presence tier (לקח מ-WebAuthn):** ל-egress הכי רגיש, ה-`.request` דורש Windows Hello gesture לפני הנפקת cap. ה-UP flag = "המשתמש נכח", לא רק "התהליך ביקש".

> **שורה תחתונה:** התכנון של יוסי קרוב מאוד לנכון. החוסרים: (1) ה-cap צריך לקודד `intent_hash` ו-`principal` מאומת, (2) צריך revocation מפורש (כמו Vault lease), (3) `audit_id` כשדה first-class. **לא** צריך JWT.

---

## 5. UX patterns — מה מוכח שעובד (ולא מעצבן)

### 5.1 1Password biometric
- מגע Touch ID / Windows Hello לפענוח. ה-UX: הסוד "נעול" עד gesture, ואז זמין לחלון קצר. ה-Shell Plugins עושים זאת per-CLI-invocation.
- **gotcha:** דורש חומרת biometric; ב-laptop בלי קורא — נופל ל-PIN/system password.
- **ל-winmux:** זה ה-tier הגבוה. לא לכל use — רק ל-egress רגיש או ל-`.request` ראשון בסשן.

### 5.2 SSH agent forwarding — אהוב ומסוכן
- **למה אהוב:** המפתח לא עוזב את המכונה המקומית; ה-remote מבקש חתימות דרך ה-socket.
- **למה מסוכן:** root על ה-remote יכול לגשת ל-socket דרך `SSH_AUTH_SOCK` ולהתחזות אליך downstream — "they aren't breaking in; they are walking through doors you opened". ProxyJump עדיף.
- **ל-winmux:** ה-`SshInject` חולק את אותו DNA — ברגע שהסוד על ה-remote, root שם שולט בו. הלקח: **אל תזריק SSH env כברירת מחדל**, רק when needed (בדיוק כמו ההמלצה "Don't turn on ForwardAgent by default").
- מקור: [SSH agent hijacking](https://www.clockwork.com/insights/ssh-agent-hijacking/), [SSH agent explained](https://smallstep.com/blog/ssh-agent-explained/).

### 5.3 Vault "approve in mobile app"
- ל-secrets בעלי-סיכון-גבוה: אישור out-of-band במכשיר נפרד.
- **ל-winmux:** overkill ל-MVP מקומי, אבל ה-feed card *הוא* ה-out-of-band approval המקומי. שדרוג עתידי: push לטלפון דרך ה-tunnel הקיים.

### 5.4 MFA fatigue — הדאטה
- Microsoft תיעדו **382,000** ניסיונות MFA fatigue ב-12 חודשים, **1% מהמשתמשים אישרו את ההתראה הראשונה הבלתי-צפויה**. ארגונים עם **number matching** ראו **ירידה של 98%** בהתקפות מוצלחות.
- **המסקנה ל-winmux:** `AlwaysAsk` על כל use → fatigue → אישור עיוור → ה-boundary קורס פסיכולוגית. **אל תשאל 50 פעם ביום.** במקום:
  - הצג **תמיד את ה-target** (URL/host/command) בכרטיס — בלי זה אי אפשר להחליט.
  - ל-egress כותב/מסוכן: **number-matching** או הקלדת אישור קצר, לא כפתור ירוק יחיד.
  - רוב ה-uses → `FirstUseWorkspace` או `TimeWindowed`, לא `AlwaysAsk`.
- מקור: [Duo: MFA fatigue](https://duo.com/blog/mfa-fatigue-what-is-it-how-to-respond), [BeyondTrust: MFA fatigue](https://www.beyondtrust.com/resources/glossary/mfa-fatigue-attack).

### 5.5 GitHub fine-grained PAT — scope-down טוב
- 50+ הרשאות granular, כל אחת no-access/read/write, repo-targeting, expiration, org-approval. "a PAT that can only read issues and do nothing else".
- **ל-winmux:** זה המודל ל-`EgressPolicy` — לא "GitHub PAT" כללי, אלא "Bearer ל-`api.github.com/user/repos` ב-GET בלבד". ה-granularity הזו = ה-blast radius הקטן שמאפשר `TimeWindowed` במקום `AlwaysAsk`.
- מקור: [Fine-grained PATs](https://github.blog/security/application-security/introducing-fine-grained-personal-access-tokens-for-github/), [Managing PATs](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).

### 5.6 המלצת granularity per egress
| egress | blast radius | default approval | מתי גם user-presence |
|---|---|---|---|
| Local shim, read-only GET (scoped url_pattern) | נמוך | `TimeWindowed(1h)` | לעולם |
| Local shim, write/mutate (POST/DELETE) | גבוה | `AlwaysAsk` + number-match | אופציונלי |
| SSH env inject | גבוה (remote-side) | `FirstUseWorkspace` per (host, secret) | לא (כבר ב-env לכל הסשן) |
| BrowserFormFill (אם אי-פעם) | גבוה (main-world) | `AlwaysAsk` + Windows Hello | **תמיד** |
| Stdin passphrase | בינוני | `AlwaysAsk` | אופציונלי |

> העיקרון: ה-approval frequency פרופורציונלי ל-blast radius, **לא** לרגישות-הסוד-המופשטת. read-only GET ל-endpoint נעול הוא low-blast גם עם סוד "רגיש".

---

## 6. ביקורת על התכנון הנוכחי של יוסי

### 6.1 Storage (`secrets.json` + `secrets.dpapi`)
- ✅ DPAPI per-secret, metadata נפרד מ-values — נכון, ומתיישב עם הקוד הקיים.
- ⚠ ה-metadata (`secrets.json`) **לא מוצפן וניתן לעריכה** ע"י same-user → tampering על policy (T-T1). זו ה-attack surface האמיתית, לא הסוד עצמו.
- ⚠ המימוש הנוכחי של DPAPI הוא **PowerShell shell-out** עם הסוד ב-env var (T-I4).
- 🔁 (a) חתום/HMAC את ה-metadata, או אחסן policy בתוך ה-DPAPI blob. (b) עבור ל-native `windows-rs` DPAPI in-process. (c) שקול TPM-sealing של ה-master כ-upgrade path.

### 6.2 Schema (`Secret` / `EgressPolicy` / `Principal` / `ApprovalPolicy`)
- ✅ ההפרדה where (`EgressPolicy`) / who (`Principal`) / how (`ApprovalPolicy`) — נקייה ונכונה.
- ⚠ `Principal::McpTool(String)` ו-`AgentInPane(PaneId)` הם claims לא-מאומתים (T-S1/T-S2).
- ⚠ אין שדה revocation/lease, אין `intent` נעול.
- 🔁 הוסף `AuthnPrincipal` נגזר-מחיבור; הוסף ל-cap את `intent_hash`+`audit_id`+revocation (סעיף 4.2).

### 6.3 API (`list` / `request` / `use`)
- ✅ שלושת ה-verbs הנכונים, וה-`egress_summary` ב-`list` (הסוכן רואה יכולת, לא ערך) — מצוין.
- ⚠ `request(principal=current_pane)` סומך על ה-claim של הקורא.
- 🔁 ה-broker גוזר את ה-principal מהחיבור; re-check ב-`use` (T-E1).

### 6.4 מימוש per-egress
- **SSH env inject:** ✅ leverage גבוה (`lib.rs:2440` כבר עושה `set_env`). ⚠ ה-`echo $X` exfil מאושר נכון כ-tradeoff. ⚠ שים לב: יש כבר **fallback שכותב env file על ה-remote** (`tunnel::write_remote_env_file`) — סוד at-rest על ה-remote, חמור יותר מ-env בלבד. 🔁 לסודות vault, לא להשתמש ב-env-file fallback; להגביל ל-`set_env` בלבד, ולתעד שזה remote-side trust.
- **Browser form fill:** ⚠⚠ **הנחת ה-isolated world שגויה** (סעיף 2.4). 🔁 או לדחות מ-MVP, או לממש דרך CDP host-side + fill-then-submit + לקבל ש-residual=High. לא לקרוא לזה "isolated".
- **HTTP header shim:** ✅ הדפוס הכי נקי (הסוד לא נכנס ל-env של הסוכן). ⚠ T-I1: השרת עשוי לשקף את ה-header ב-body → צריך output scrubbing. 🔁 הוסף censoring על ה-stdout לפני החזרה ל-LLM.
- **Stdin passphrase:** ✅/⚠ התכנון קורא לזה weakest link — **מסכים חלקית, אבל מהסיבה ההפוכה.** Stdin דווקא *לא רע* (הסוד נכנס ל-stdin של תהליך אחד, לא ל-env שגלוי ל-`ps e`). ה-weakest link האמיתי הוא **SSH env** (raw value ב-remote env, `echo` מדליף, env-file fallback). Stdin פשוט *מוגבל* (רק תהליכים שקוראים passphrase מ-prompt). 🔁 שמור Stdin כ-tier-2, אבל אל תתייג אותו "החלש ביותר" — ה-SSH env הוא.

### 6.5 Feed cards
- ✅ שימוש חוזר ב-`feed.push` הקיים — נכון.
- ⚠ הכרטיס לא מציג את ה-**intent המלא** באופן שמונע fatigue (סעיף 5.4); אין הגנת flood (T-D1).
- 🔁 הצג target בולט, הוסף number-matching ל-write ops, rate-limit + תקרת pending.

### 6.6 Audit log (SQLite)
- ✅ הסכמה (ts/pane/secret/egress/target/decision/cap_id/used) — מקיפה.
- ⚠ same-user-writable → repudiation (T-R1); אין tamper-evidence.
- 🔁 hash-chain + עיגון head; rusqlite מספיק (לא SQLCipher) — הבעיה integrity, לא confidentiality (סעיף 2.5).

### 6.7 הביקורת על שלוש הנקודות שיוסי ביקש לבדוק במפורש
1. **Stdin = weakest link?** — **חולק.** ראה 6.4. Stdin מוגבל אבל לא החלש; SSH env הוא.
2. **SSH env `echo $X` — tradeoff מקובל או יש דפוס נקי יותר?** — מקובל **רק** למקרה "agent על remote שצריך env tool (gh/aws)". לדפוס נקי יותר מקומית: **PTY-side `read -s` interception** אפשרי תיאורטית אבל שביר (תלוי-shell, race עם ה-prompt) — לא שווה ל-MVP. הדפוס הנקי הוא **לא להשתמש ב-env בכלל** מקומית, אלא ב-child-process shim. SSH env נשאר רק כשהכלי *חייב* env על ה-remote.
3. **BrowserFormFill — attack surface אמיתי?** — גדול מהמתואר. דף זדוני יכול **לזהות שהמילוי קרה** (input/change events, MutationObserver, setter override) ולקרוא את הערך, כי הכל ב-main world. זה לא "בינוני" — זה High לדף לא-מהימן.

---

## 7. MVP מומלץ — מה לשלוח קודם

**עקרונות הבחירה:** (1) workflow אמיתי שיאומץ, (2) מאמת capability > credential, (3) לא צובע לפינה ארכיטקטונית.

### 7.1 מה נכנס ל-MVP (שתי egress)
1. **Local child-process shim — `winmux exec --with-secret <cap_id> -- <cmd>`.**
   - *למה:* מאמת את התזה הכי חזק — הסוד **אף פעם** לא נכנס ל-env של הסוכן ולא לצ'אט. ה-shim פותר cap_id → סוד, מזריק ל-env של ה-child בלבד (או ל-header אם HTTP), מחזיר output מצונזר. workflow: `gh`, `curl`, `aws` מקומית.
   - *blast radius:* נמוך, scoped ל-`command_pattern`+`url_pattern`.
2. **SSH env injection.**
   - *למה:* 90% כבר קיים (`lib.rs:2440`). workflow: agent על remote מריץ `gh pr create`. leverage מיידי.
   - *אזהרה מתועדת:* remote-side trust, `echo` מדליף, **בלי env-file fallback** לסודות vault.

### 7.1.1 איפה הסוד חי בכל egress (data-flow)

הטבלה הזו היא מבחן ה-"capability > credential": ככל שהסוד חי בפחות מקומות וקרוב יותר ל-broker בלבד — כך הדפוס נקי יותר.

| egress | סוד ב-broker mem | סוד ב-agent env | סוד ב-LLM context | סוד at-rest מחוץ ל-vault | ניקיון |
|---|---|---|---|---|---|
| Local shim (env/header) | ✅ רגעית | ❌ (ב-child בלבד) | ❌ | ❌ | **הכי נקי** |
| SSH env inject | ✅ רגעית | ✅ (remote env) | ❌ | ⚠ env-file fallback על remote | בינוני |
| HTTP header (=shim) | ✅ רגעית | ❌ | ❌ (אם scrubbing) | ❌ | נקי |
| BrowserFormFill | ✅ רגעית | ❌ | ❌ | ❌ (אבל main-world readable) | שבור ב-WebView2 |
| Stdin passphrase | ✅ רגעית | ❌ | ❌ | ❌ | נקי-אך-מוגבל |

> ה-shim המקומי הוא היחיד שבו הסוד **אף פעם** לא יוצא מ-(broker → child). זו הסיבה שהוא ה-egress שמאמת את התזה הכי חזק, ולכן הוא #1 ב-MVP.

### 7.1.2 תרחיש זרימה מלא (happy path + denied)

**Happy path — agent מריץ `gh pr list` מקומית:**
```
1. agent → MCP: list_secrets(pane=#3)
   broker → agent: [{ id: "s-gh", name: "GitHub PAT (prod)",
                      egress: "env GH_TOKEN for `gh *`" }]   // ערך לא נחשף
2. agent → MCP: request_capability("s-gh",
                  intent={ egress: EnvVar, command: "gh pr list", env: "GH_TOKEN" })
   broker: principal נגזר מהחיבור (pane #3, מאומת) ✓
           intent ⊆ EgressPolicy של s-gh ✓ (command_pattern="gh *")
           ApprovalPolicy = FirstUseWorkspace, וזה ה-use הראשון → feed card
   broker → agent: Pending(feed_id=42)
   broker → audit: { ts, pane=#3, secret=s-gh, egress=EnvVar,
                     target="gh pr list", decision=pending, cap_id=null }
3. user: לוחץ [Allow for workspace] בכרטיס #42
   broker: יוצר PendingCap{ cap_id=<128-bit>, intent_hash, audit_id, uses_left=1, +60s }
   broker → audit: עדכון decision=allow_window, cap_id=<..>
4. agent → MCP: use_capability(cap_id, payload={ argv: ["gh","pr","list"] })
   broker: payload תואם intent_hash ✓, principal עדיין pane #3 ✓
           DPAPI unwrap (native, in-proc) → SecretString
           spawn child עם env GH_TOKEN=<secret>, מאפס SecretString
           scrub stdout מפני ערך הסוד
   broker → agent: { result_handle, stdout(scrubbed), exit=0 }
   broker → audit: used=true   // chain hash מתעדכן
```

**Denied path — pane אחר מבקש את אותו סוד:**
```
agent(pane #7) → MCP: request_capability("s-gh", ...)
broker: principal=pane #7, אבל Principal של s-gh מתיר רק pane #3 → Denied
broker → audit: { pane=#7, decision=deny }   // נרשם גם דחייה
broker → agent: Denied("secret not scoped to this principal")
```

זה ממחיש את שלוש ההגנות: principal **נגזר** (לא claimed), intent **נעול** מ-request ל-use, וכל החלטה (כולל deny ו-pending) **נרשמת** בשרשרת ה-audit.

### 7.2 מה נדחה מ-MVP
- **BrowserFormFill** — isolation שבור ב-WebView2 (2.4). דורש מחקר CDP-host-side נפרד. סיכון גבוה, ערך לא ברור ל-MVP.
- **Stdin passphrase** — niche (רק כלים שקוראים prompt), ערך נמוך ל-coding workflow.
- **HTTP header כ-egress נפרד** — מתמזג עם ה-shim (`--with-secret` יכול להזריק header במקום env). לא צריך egress נפרד.

### 7.3 רכיבי תשתית שחייבים להיכנס ל-MVP (לא אופציונלי)
- DPAPI **native in-process** (`windows-rs`), לא PowerShell shell-out.
- **peer authentication** של ה-MCP/pane (named-pipe ACL + derive principal מחיבור) — בלעדיו ה-Spoofing/EoP פתוחים.
- **output scrubbing** על ה-shim (T-I1).
- **audit hash-chain** (rusqlite) — אפילו בלי עיגון-head מלא, השרשרת לבדה כבר מזהה tampering בסיסי.
- cap = opaque handle, single-use, 60s, עם `intent_hash`+`audit_id` (סעיף 4.2).

### 7.4 מה נדחה ל-hardening (post-MVP)
- TPM-sealing של ה-master (שובר את ה-intra-user weakness).
- Windows Hello user-presence ל-egress רגיש.
- עיגון head של ה-audit chain ב-TPM/append-only.
- `secrecy::SecretString` + `VirtualLock` בכל המסלולים.
- BrowserFormFill דרך CDP (אם בכלל).

### 7.5 הערכת זמנים
| תוספת | תיאור | ימים |
|---|---|---|
| **MVP** | Schema+DPAPI native, RPC list/request/use, peer-authn, feed integration, SSH env inject, local shim, output scrubbing, audit hash-chain, MCP tools | **6-7** |
| Increment 1 | TPM-sealing + Windows Hello user-presence ל-egress רגיש | 3-4 |
| Increment 2 | audit head anchoring + revocation UI + rate-limiting/flood guard | 2-3 |
| Increment 3 | BrowserFormFill דרך CDP host-side (אם מחקר ה-CDP מצדיק) | 4-5 |

(התכנון המקורי העריך MVP ב-5 ימים; ה-+1-2 הם ה-peer-authn + scrubbing + hash-chain שאני מוסיף כ-non-negotiable.)

---

## 8. מה אנחנו יודעים / לא יודעים / לחקור הלאה

### 8.1 מה אנחנו יודעים עכשיו
- **capability > credential מאומת בשוק** — Claude Code `apiKeyHelper`, Continue.dev proxy, Infisical/Doppler `run`. התזה של יוסי נכונה.
- **DPAPI = inter-user isolation בלבד.** ה-vault מגן על ה-LLM channel, לא על same-user code. זו עובדה קובעת-ארכיטקטורה, לא פרט.
- **WebView2 חסר isolated worlds.** ה-BrowserFormFill כפי שתוכנן לא בטוח. עובדה מאומתת מ-Microsoft docs + WebView2Feedback.
- **MFA fatigue הוא איום ממשי** (1% מאשרים עיוור). `AlwaysAsk` על הכל קורס פסיכולוגית.
- **cap צריך להיות opaque handle**, לא JWT. ה-broker המקומי הוא ה-verifier היחיד.
- **audit צריך integrity (hash-chain), לא confidentiality (SQLCipher).**
- **ה-weakest link הוא SSH env, לא Stdin** — בניגוד לתכנון.

### 8.2 מה אנחנו עדיין לא יודעים
- **האם CDP host-side fill באמת בטוח יותר?** לא בדקתי בפועל אם דף יכול לזהות `Input.insertText` דרך CDP כמו שהוא מזהה JS injection. דורש PoC.
- **כמה דליף `secrecy`/`zeroize` בפועל מול hibernate?** ה-docs אומרים שאין mlock; לא מדדתי כמה זמן סוד שורד ב-`hiberfil.sys`.
- **עלות ה-TPM gesture ב-UX.** האם PIN-per-egress יוצר fatigue משלו? אין דאטה ספציפי ל-developer workflow (להבדיל מ-enterprise login).
- **האם peer-authn דרך image-path מספיק** מול same-user שמזייף path? לא ברור בלי code-signing enforcement.
- **performance של native DPAPI** ב-loop של עשרות uses — לא נמדד.

### 8.3 מה לחקור הלאה (לפני implementation)
1. **PoC: WebView2 CDP fill** — לבנות דף עדני שמנסה לזהות/לקרוא fill דרך CDP מול JS injection. מכריע אם BrowserFormFill בכלל ישים.
2. **PoC: named-pipe peer-authn** — `GetNamedPipeClientProcessId` → verify image path/signature. למדוד עקיפה ב-same-user.
3. **TPM-sealing spike** — `NCryptCreatePersistedKey` + TPM key provider, לעטוף את ה-DPAPI master. למדוד UX של PIN-per-unlock.
4. **output scrubbing robustness** — איך מצנזרים סוד שעבר base64/url-encode/JSON-escape בתשובת שרת? regex פשוט לא יספיק (ראה כישלון Warp).
5. **audit head anchoring** — לבדוק `FILE_APPEND_DATA`-only ACL מול TPM monotonic counter כעוגן.
6. **`read -s` PTY interception** — האם בכלל אפשרי אמין ל-SSH, או לזנוח לטובת shim בלבד.

---

## Sources

**כלי AI / טרמינל:**
- [Claude Code — Environment variables](https://code.claude.com/docs/en/env-vars)
- [Claude Code — Manage API key env vars](https://support.claude.com/en/articles/12304248-manage-api-key-environment-variables-in-claude-code)
- [Warp — Secret Redaction](https://docs.warp.dev/privacy/secret-redaction)
- [Warp — Don't accidentally leak secrets](https://www.warp.dev/blog/dont-accidentally-leak-secrets-from-your-terminal)
- [OpenHands — Secret Registry](https://docs.openhands.dev/sdk/guides/secrets)
- [OpenHands — Mitigating prompt injection](https://openhands.dev/blog/mitigating-prompt-injection-attacks-in-software-agents)
- [OpenHands — Issue #9124 (env var handling)](https://github.com/OpenHands/OpenHands/issues/9124)
- [Aider — Config with .env](https://aider.chat/docs/config/dotenv.html)
- [Aider — API Keys](https://aider.chat/docs/config/api-keys.html)
- [Cursor security — Infisical](https://infisical.com/blog/secure-secrets-management-for-cursor-cloud-agents)
- [Your AI agent is reading your .env — Infisical](https://infisical.com/blog/your-ai-coding-agent-is-reading-your-env-file)
- [Cursor git-hooks RCE — Hackread](https://hackread.com/cursor-ai-ide-vulnerability-code-execution-git-hooks/)
- [Continue.dev — Secret Types](https://docs.continue.dev/mission-control/secrets/secret-types)
- [GitHub Copilot CLI — Authentication](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli)

**Secrets managers:**
- [1Password — Biometric unlock](https://developer.1password.com/docs/cli/use-biometric-unlock/)
- [1Password — Shell Plugins](https://1password.com/blog/shell-plugins)
- [HashiCorp Vault — Lease, Renew, Revoke](https://developer.hashicorp.com/vault/docs/concepts/lease)
- [HashiCorp Vault — AppRole](https://developer.hashicorp.com/vault/docs/auth/approle)
- [AWS STS — AssumeRole](https://docs.aws.amazon.com/STS/latest/APIReference/API_AssumeRole.html)
- [Infisical — CLI secrets](https://infisical.com/docs/cli/commands/secrets)
- [age — Authentication (Filippo Valsorda)](https://words.filippo.io/age-authentication/)
- [FiloSottile/age](https://github.com/FiloSottile/age)

**Windows building blocks:**
- [DpapiDataProtector.Scope — Microsoft Learn](https://learn.microsoft.com/en-us/dotnet/api/system.security.cryptography.dpapidataprotector.scope)
- [CryptProtectData — Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)
- [The Downfall of DPAPI — Sygnia](https://www.sygnia.co/blog/the-downfall-of-dpapis-top-secret-weapon/)
- [Dumping stored credentials — Tyranid's Lair](https://www.tiraniddo.dev/2021/05/dumping-stored-credentials-with.html)
- [WebView2 — Develop secure apps (Microsoft Learn)](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/security)
- [WebView2 context isolation — Feedback #2510](https://github.com/MicrosoftEdge/WebView2Feedback/issues/2510)
- [Virtual Smart Card overview — Microsoft Learn](https://learn.microsoft.com/en-us/windows/security/identity-protection/virtual-smart-cards/virtual-smart-card-overview)
- [Windows Hello — Microsoft Learn](https://learn.microsoft.com/en-us/windows/apps/develop/security/windows-hello)
- [zeroize — docs.rs](https://docs.rs/zeroize/latest/zeroize/)
- [secrecy — docs.rs](https://docs.rs/secrecy/latest/secrecy/)
- [YubiKey SSH auth on Windows — Yubico](https://developers.yubico.com/PGP/SSH_authentication/Windows.html)
- [Tamper-evident audit log w/ SHA-256 hash chains](https://dev.to/veritaschain/building-a-tamper-evident-audit-log-with-sha-256-hash-chains-zero-dependencies-h0b)
- [Crosby & Wallach — Tamper-Evident Logging (USENIX Security '09)](https://static.usenix.org/event/sec09/tech/full_papers/crosby.pdf)

**Capability / UX:**
- [WebAuthn assertion — Corbado glossary](https://www.corbado.com/glossary/assertion)
- [AuthenticatorAssertionResponse — MDN](https://developer.mozilla.org/en-US/docs/Web/API/AuthenticatorAssertionResponse)
- [GitHub fine-grained PATs — GitHub Blog](https://github.blog/security/application-security/introducing-fine-grained-personal-access-tokens-for-github/)
- [Managing PATs — GitHub Docs](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens)
- [SSH agent hijacking — Clockwork](https://www.clockwork.com/insights/ssh-agent-hijacking/)
- [SSH agent explained — smallstep](https://smallstep.com/blog/ssh-agent-explained/)
- [MFA fatigue — Duo](https://duo.com/blog/mfa-fatigue-what-is-it-how-to-respond)
- [MFA fatigue attack — BeyondTrust](https://www.beyondtrust.com/resources/glossary/mfa-fatigue-attack)

---

*נכתב כמחקר רקע ל-Secrets Vault (#3.2). מבוסס על התכנון ב-`docs/COMPETITIVE-SCAN.md` והקוד הקיים ב-`provisioning.rs` / `lib.rs`. השלב הבא: יוסי קורא, ואז ADR + spec לפני implementation.*
