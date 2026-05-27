# إعداد مفتاح SSH يدويًا

استخدم هذا الدليل إذا كنت تفضّل إعداد مصادقة SSH بالمفتاح يدويًا، أو إذا لم ينجح الإعداد التلقائي.

## الخطوة 1 — أنشئ مفتاحًا على جهازك

في Windows (PowerShell):
```powershell
ssh-keygen -t ed25519 -f $HOME\.ssh\winmux_key -C "winmux-<server-name>"
```

في macOS أو Linux:
```bash
ssh-keygen -t ed25519 -f ~/.ssh/winmux_key -C "winmux-<server-name>"
```

عندما يُطلب منك إدخال passphrase، اضغط Enter لتركها فارغة، أو حدد واحدة لمزيد من الأمان.

## الخطوة 2 — انسخ مفتاحك العام

في Windows (PowerShell):
```powershell
Get-Content $HOME\.ssh\winmux_key.pub | Set-Clipboard
```

في macOS:
```bash
pbcopy < ~/.ssh/winmux_key.pub
```

في Linux:
```bash
cat ~/.ssh/winmux_key.pub
```

## الخطوة 3 — ثبّت المفتاح على الخادم

اتصل بخادمك (بأي طريقة تعمل — SSH بكلمة مرور، web console، طرفية مزود السحابة)، ثم نفّذ:

```bash
mkdir -p ~/.ssh
chmod 700 ~/.ssh
echo "PASTE_YOUR_PUBLIC_KEY_HERE" >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

استبدل `PASTE_YOUR_PUBLIC_KEY_HERE` بالمفتاح العام من الخطوة 2. المفتاح بأكمله هو سطر واحد يبدأ بـ `ssh-ed25519`.

إذا كان لديك `ssh-copy-id`، يمكنك دمج الخطوتين 2 و 3:
```bash
ssh-copy-id -i ~/.ssh/winmux_key.pub user@server.example.com
```

## الخطوة 4 — اضبط winmux

افتح إعدادات بيئة العمل واضبط مسار مفتاح SSH على `~/.ssh/winmux_key` (أو حيث حفظت مفتاحك الخاص).

## استكشاف الأخطاء

- **Permission denied (publickey)**: تحقق من صلاحيات الملفات — `~/.ssh` يجب أن يكون 700، `authorized_keys` يجب أن يكون 600، المفتاح الخاص يجب أن يكون 600.
- **مستخدم خاطئ على الخادم**: تأكد من أنك أضفت المفتاح إلى `~/.ssh/authorized_keys` للمستخدم الصحيح، وليس root افتراضيًا.
- **مفاتيح متعددة**: SSH يجرّب المفاتيح بالترتيب؛ حدّد واحدًا باستخدام `ssh -i ~/.ssh/winmux_key user@host`.
