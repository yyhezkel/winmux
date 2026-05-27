# הקמה ידנית של מפתח SSH

השתמש במדריך הזה אם אתה מעדיף להגדיר התחברות SSH במפתח באופן ידני, או אם ההגדרה האוטומטית לא עבדה.

## שלב 1 — צור מפתח על המחשב שלך

ב-Windows (PowerShell):
```powershell
ssh-keygen -t ed25519 -f $HOME\.ssh\winmux_key -C "winmux-<server-name>"
```

ב-macOS או Linux:
```bash
ssh-keygen -t ed25519 -f ~/.ssh/winmux_key -C "winmux-<server-name>"
```

כשתתבקש לסיסמת מפתח (passphrase), לחץ Enter בלי להזין כלום אם אינך רוצה אחת, או הגדר אחת לאבטחה נוספת.

## שלב 2 — העתק את המפתח הציבורי שלך

ב-Windows (PowerShell):
```powershell
Get-Content $HOME\.ssh\winmux_key.pub | Set-Clipboard
```

ב-macOS:
```bash
pbcopy < ~/.ssh/winmux_key.pub
```

ב-Linux:
```bash
cat ~/.ssh/winmux_key.pub
```

## שלב 3 — התקן את המפתח על השרת

התחבר לשרת (בכל דרך שעובדת — SSH עם סיסמה, web console, מסוף של ספק הענן), ואז הרץ:

```bash
mkdir -p ~/.ssh
chmod 700 ~/.ssh
echo "PASTE_YOUR_PUBLIC_KEY_HERE" >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

החלף את `PASTE_YOUR_PUBLIC_KEY_HERE` במפתח הציבורי משלב 2. כל המפתח הוא שורה אחת שמתחילה ב-`ssh-ed25519`.

אם יש לך `ssh-copy-id`, אפשר לאחד את שלבים 2 ו-3:
```bash
ssh-copy-id -i ~/.ssh/winmux_key.pub user@server.example.com
```

## שלב 4 — הגדר את winmux

פתח את הגדרות סביבת העבודה והגדר את הנתיב למפתח SSH כ-`~/.ssh/winmux_key` (או היכן ששמרת את המפתח הפרטי).

## פתרון בעיות

- **Permission denied (publickey)**: ודא הרשאות קבצים — `~/.ssh` צריך להיות 700, `authorized_keys` צריך להיות 600, המפתח הפרטי צריך להיות 600.
- **משתמש שגוי בשרת**: ודא שהוספת את המפתח ל-`~/.ssh/authorized_keys` של המשתמש הנכון, לא ל-root כברירת מחדל.
- **מספר מפתחות**: SSH מנסה מפתחות לפי הסדר; ציין מפתח ספציפי עם `ssh -i ~/.ssh/winmux_key user@host`.
